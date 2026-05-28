use std::{collections::HashMap, sync::Arc};

use super::Wafer;
#[cfg(feature = "wasm")]
use super::{parse_unversioned_block, parse_versioned_block, RegistryManifest, ABI_VERSION};
#[cfg(feature = "wasm")]
use crate::block::Block;
use crate::error::{BlockReferenceError, BlockReferenceSource, RuntimeError};

impl Wafer {
    /// Gather `"uses"` contributions from all block configs and deep-merge them
    /// into the target infrastructure block configs.
    pub(crate) fn expand_composite_configs(&mut self) {
        let keys: Vec<String> = self
            .block_configs
            .keys()
            .filter(|k| self.config_expanders.contains_key(k.as_str()))
            .cloned()
            .collect();

        for key in keys {
            if let Some(config) = self.block_configs.remove(&key) {
                if let Some(expander) = self.config_expanders.get(&key) {
                    for (name, val) in expander(config) {
                        let entry = self
                            .block_configs
                            .entry(name)
                            .or_insert_with(|| serde_json::Value::Object(Default::default()));
                        super::deep_merge(entry, &val);
                    }
                }
            }
        }
    }

    /// Expand declarative `config_map` and `config_defaults` from WaferFlow definitions.
    pub(crate) fn expand_declarative_flow_configs(&mut self) {
        #[expect(
            clippy::type_complexity,
            reason = "tuple mirrors the accumulated shape of the eligible iterator; no useful alias"
        )]
        let eligible: Vec<(
            String,
            HashMap<String, wafer_flow::ConfigMapEntry>,
            HashMap<String, serde_json::Value>,
        )> = self
            .flows
            .values()
            .filter(|f| f.config_map.as_ref().is_some_and(|m| !m.is_empty()))
            .filter(|f| self.block_configs.contains_key(&f.id))
            .map(|f| {
                (
                    f.id.clone(),
                    f.config_map.clone().unwrap_or_default(),
                    f.config_defaults.clone().unwrap_or_default(),
                )
            })
            .collect();

        for (flow_id, config_map, config_defaults) in eligible {
            let Some(flow_config) = self.block_configs.remove(&flow_id) else {
                continue;
            };

            // 1. Apply config_defaults to target blocks
            for (target, defaults) in &config_defaults {
                let entry = self
                    .block_configs
                    .entry(target.clone())
                    .or_insert_with(|| serde_json::Value::Object(Default::default()));
                super::deep_merge(entry, defaults);
            }

            // 2. Route config_map keys to target blocks
            if let Some(obj) = flow_config.as_object() {
                for (user_key, mapping) in &config_map {
                    if let Some(value) = obj.get(user_key) {
                        let entry = self
                            .block_configs
                            .entry(mapping.target.clone())
                            .or_insert_with(|| serde_json::Value::Object(Default::default()));
                        let contribution =
                            serde_json::json!({ mapping.key.clone(): value.clone() });
                        super::deep_merge(entry, &contribution);
                    }
                }
            }
        }
    }

    pub(crate) fn gather_uses_configs(&mut self) {
        let mut contributions: Vec<(String, serde_json::Value)> = Vec::new();

        for config in self.block_configs.values() {
            if let Some(uses) = config.get("uses").and_then(|v| v.as_object()) {
                for (target, contrib) in uses {
                    contributions.push((target.clone(), contrib.clone()));
                }
            }
        }

        for (target, contrib) in contributions {
            let resolved_target = self.aliases.get(&target).cloned().unwrap_or(target);
            let entry = self
                .block_configs
                .entry(resolved_target)
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            super::deep_merge(entry, &contrib);
        }

        for config in self.block_configs.values_mut() {
            if let Some(obj) = config.as_object_mut() {
                obj.remove("uses");
            }
        }
    }

    /// Finalize runtime configuration before serving traffic.
    ///
    /// `seal()` performs the once-per-boot operations that lazy init does
    /// **not** subsume:
    ///
    /// 1. Resolve remote entries (download `.flow.json` / `.wasm` for
    ///    deferred registrations).
    /// 2. Expand composite configs (e.g. `wafer-run/http-server` →
    ///    `http-listener` + `router`).
    /// 3. Expand declarative flow `config_map` / `config_defaults`.
    /// 4. Gather `"uses"` contributions across all block configs.
    /// 5. Compute effective capabilities per block (declared ∩ config ∩ host)
    ///    and propagate them into each block.
    /// 6. Resolve remote blocks referenced by flow steps and (PR B) router
    ///    routes. Aggregates every missing reference into one
    ///    `RuntimeError::BlocksNotFound` so operators see the full punch
    ///    list with each missing block's source.
    /// 7. Finalize the [`crate::snapshot::StartupSnapshot`] consumed by
    ///    every [`crate::runtime::RuntimeContext`].
    ///
    /// Block `Init` lifecycle events are **not** dispatched here. Each block
    /// is initialized on first dispatch via [`Wafer::init_block`] (lazy
    /// once-success). Required-config presence is **not** validated here —
    /// broken paths surface as 5xx on first invocation. Use
    /// [`Wafer::validate_all_block_configs`] for proactive health checks.
    pub async fn seal(&mut self) -> Result<(), RuntimeError> {
        // Drain the grant-validation accumulator before sealing. This is the
        // common boot funnel: both `start_with_priority()` (native) and direct
        // `seal()` callers (Cloudflare Workers, browser WASM) pass through here.
        // If any typed grants were rejected during register_block / set_admin_block,
        // refuse boot with all rejections listed in one error.
        if !self.grant_validation_errors.is_empty() {
            let errors = std::mem::take(&mut self.grant_validation_errors);
            return Err(RuntimeError::GrantsRejected(errors));
        }

        // 1. Resolve remote entries: download .flow.json / .wasm for deferred registrations
        #[cfg(feature = "wasm")]
        self.resolve_remote_entries().await?;

        // 2. Expand composite configs (e.g. wafer-run/http-server → http-listener + router)
        self.expand_composite_configs();
        // 3. Expand declarative flow config_map / config_defaults
        self.expand_declarative_flow_configs();
        // 4. Gather uses contributions
        self.gather_uses_configs();

        // 5. Compute effective capabilities per block: declared ∩ config ∩ host.
        // Also strip the reserved `capabilities` subkey from the block config
        // so it doesn't leak into `ctx.config_get(...)`.
        {
            let mut eff: std::collections::HashMap<String, wafer_block::BlockCapabilities> =
                std::collections::HashMap::new();
            for (name, block) in &self.blocks {
                let declared = block
                    .info()
                    .capabilities
                    .unwrap_or_else(wafer_block::BlockCapabilities::unrestricted);

                // Strip + parse the `capabilities` subkey from block config.
                let config_overrides = if let Some(cfg) = self.block_configs.get_mut(name) {
                    if let Some(obj) = cfg.as_object_mut() {
                        if let Some(raw) = obj.remove("capabilities") {
                            match serde_json::from_value::<
                                wafer_block::capabilities::ConfigCapabilityOverrides,
                            >(raw)
                            {
                                Ok(o) => o,
                                Err(e) => {
                                    tracing::warn!(
                                        block = %name,
                                        error = %e,
                                        "failed to parse `capabilities` subkey — ignoring"
                                    );
                                    wafer_block::capabilities::ConfigCapabilityOverrides::default()
                                }
                            }
                        } else {
                            wafer_block::capabilities::ConfigCapabilityOverrides::default()
                        }
                    } else {
                        wafer_block::capabilities::ConfigCapabilityOverrides::default()
                    }
                } else {
                    wafer_block::capabilities::ConfigCapabilityOverrides::default()
                };

                let effective = declared.apply_config_overrides(&config_overrides);

                // Warn on widening attempts (fields where config > declared).
                log_widening_attempts(name, &config_overrides, &effective);

                // Propagate effective caps into the block for runtime enforcement.
                // Native blocks ignore this call (default no-op); WASM blocks update
                // their interior-mutable capabilities field so that every subsequent
                // host-import check and sanitizer uses the narrowed effective set.
                block.runtime_capabilities_mut(effective.clone());

                eff.insert(name.clone(), effective);
            }
            self.effective_capabilities = std::sync::Arc::new(eff);
        }

        // 6. Resolve remote blocks referenced by flow steps and router routes.
        // Collect every reference + its source, then resolve-or-aggregate-fail.
        //
        // PR A lands the flow-step half of the walk. PR B (Wave 16) extends
        // the collection to also include router routes via
        // `router_walk::collect_router_route_refs`.
        let mut references: HashMap<String, Vec<BlockReferenceSource>> = HashMap::new();

        for (flow_id, flow) in &self.flows {
            for (step_index, step) in flow.steps.iter().enumerate() {
                let canonical = self
                    .aliases
                    .get(&step.block)
                    .cloned()
                    .unwrap_or_else(|| step.block.clone());
                references
                    .entry(canonical)
                    .or_default()
                    .push(BlockReferenceSource::Flow {
                        flow_id: flow_id.clone(),
                        step_index,
                        step_id: step.id.clone(),
                    });
            }
        }
        // PR B inserts the router-route collection here:
        //     for (canonical, source) in router_walk::collect_router_route_refs(self) {
        //         references.entry(canonical).or_default().push(source);
        //     }

        let mut not_found: Vec<BlockReferenceError> = Vec::new();
        for (canonical, sources) in references {
            if self.blocks.contains_key(&canonical) {
                continue;
            }
            #[cfg(feature = "wasm")]
            {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .map_err(|e| {
                        RuntimeError::Registry(format!("failed to create HTTP client: {e}"))
                    })?;
                if let Some(block) = self.resolve_remote_block(&client, &canonical).await? {
                    tracing::info!(block = %canonical, "downloaded remote block");
                    self.register_remote_block(&canonical, block)?;
                    continue;
                }
            }
            not_found.push(BlockReferenceError {
                name: canonical,
                sources,
            });
        }

        if !not_found.is_empty() {
            return Err(RuntimeError::BlocksNotFound(not_found));
        }

        // 7. Finalize the startup snapshot. Block configs survive in
        // `self.block_configs` and are mirrored here for context consumers.
        // (Lazy init reads from the runtime's `ConfigSource` — not from
        // `self.block_configs` — when dispatching `lifecycle(Init)` on
        // first request; see `run_init_pipeline`.)
        self.rebuild_all_blocks();
        self.snapshot = Arc::new(crate::snapshot::StartupSnapshot {
            blocks: super::lifecycle::sorted_snapshot(self.blocks.values().map(|b| b.info())),
            flow_infos: self.flows_info(),
            flow_defs: self.flow_defs(),
            block_configs: self.block_configs.clone(),
            interface_specs: self.interface_specs.values().cloned().collect(),
        });

        Ok(())
    }

    /// Resolve remote blocks for deferred registrations via the registry.
    #[cfg(feature = "wasm")]
    async fn resolve_remote_entries(&mut self) -> Result<(), RuntimeError> {
        let candidates: Vec<String> = self
            .block_configs
            .keys()
            .filter(|name| name.contains('/'))
            .filter(|name| !self.flows.contains_key(name.as_str()))
            .filter(|name| !self.blocks.contains_key(name.as_str()))
            .filter(|name| {
                parse_unversioned_block(name).is_some() || parse_versioned_block(name).is_some()
            })
            .cloned()
            .collect();

        if candidates.is_empty() {
            return Ok(());
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| RuntimeError::Registry(format!("failed to create HTTP client: {e}")))?;

        for name in candidates {
            let Some(remote_ref) =
                parse_versioned_block(&name).or_else(|| parse_unversioned_block(&name))
            else {
                continue;
            };

            let manifest_url = format!(
                "https://raw.githubusercontent.com/wafer-run/registry/main/{}/{}/manifest.json",
                remote_ref.org, remote_ref.block
            );

            let resp = client
                .get(&manifest_url)
                .header("User-Agent", "wafer-run/0.1.0")
                .send()
                .await
                .map_err(|e| {
                    RuntimeError::Registry(format!(
                        "failed to fetch registry manifest for {name}: {e}"
                    ))
                })?;

            if resp.status().as_u16() == 404 {
                continue;
            }
            if resp.status().as_u16() != 200 {
                return Err(RuntimeError::Registry(format!(
                    "failed to fetch registry manifest for {}: HTTP {}",
                    name,
                    resp.status().as_u16()
                )));
            }

            let manifest_bytes = resp.bytes().await.map_err(|e| {
                RuntimeError::Registry(format!("failed to read manifest for {name}: {e}"))
            })?;
            let manifest: RegistryManifest =
                serde_json::from_slice(&manifest_bytes).map_err(|e| {
                    RuntimeError::Registry(format!(
                        "failed to parse registry manifest for {name}: {e}"
                    ))
                })?;

            let version = if remote_ref.version == "latest" {
                manifest.latest.clone()
            } else {
                remote_ref.version.clone()
            };

            let entry = manifest.versions.get(&version).ok_or_else(|| {
                RuntimeError::Registry(format!(
                    "version {version} not found in registry for {name}"
                ))
            })?;

            if entry.abi != ABI_VERSION {
                return Err(RuntimeError::AbiMismatch {
                    name: name.clone(),
                    required: entry.abi,
                    supported: ABI_VERSION,
                });
            }

            if let Some(flow_url) = &entry.flow_url {
                let flow = self
                    .download_flow_from_url(&client, flow_url, &name)
                    .await?;

                // Pre-resolve block dependencies from the flow's blocks list
                let blocks_to_resolve: Vec<String> = flow
                    .blocks
                    .as_ref()
                    .map(|b| {
                        b.iter()
                            .filter(|b| !self.blocks.contains_key(b.as_str()))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();

                self.add_flow(flow);

                for block_name in &blocks_to_resolve {
                    if self.blocks.contains_key(block_name.as_str()) {
                        continue;
                    }
                    match self.resolve_remote_block(&client, block_name).await {
                        Ok(Some(block)) => {
                            tracing::info!(block = %block_name, "downloaded remote block");
                            self.register_remote_block(block_name, block)?;
                        }
                        Ok(None) => {
                            tracing::debug!(
                                block = %block_name,
                                "block not found in registry, will resolve during step resolution"
                            );
                        }
                        Err(e) => {
                            return Err(RuntimeError::Registry(format!(
                                "failed to download block dependency {block_name:?} for flow {name:?}: {e}"
                            )));
                        }
                    }
                }
            } else if let Some(wasm_url) = &entry.wasm_url {
                let block = self
                    .download_wasm_from_url(&client, wasm_url, &name)
                    .await?;
                tracing::info!(block = %name, "downloaded remote WASM block from registry");
                self.register_remote_block(&name, block)?;
            }
        }

        Ok(())
    }

    /// Download a `.flow.json` from a direct URL and parse as WaferFlow.
    #[cfg(feature = "wasm")]
    async fn download_flow_from_url(
        &self,
        client: &reqwest::Client,
        url: &str,
        name: &str,
    ) -> Result<wafer_flow::WaferFlow, RuntimeError> {
        let resp = client
            .get(url)
            .header("User-Agent", "wafer-run/0.1.0")
            .send()
            .await
            .map_err(|e| RuntimeError::Flow(format!("failed to download flow for {name}: {e}")))?;

        if resp.status().as_u16() != 200 {
            return Err(RuntimeError::Flow(format!(
                "failed to download flow for {}: HTTP {}",
                name,
                resp.status().as_u16()
            )));
        }

        let body = resp
            .bytes()
            .await
            .map_err(|e| RuntimeError::Flow(format!("failed to read flow body for {name}: {e}")))?;

        let body_str = std::str::from_utf8(&body).map_err(|e| {
            RuntimeError::Flow(format!("failed to decode flow body for {name}: {e}"))
        })?;

        let flow = wafer_flow::parse(body_str).map_err(|e| {
            RuntimeError::Flow(format!("failed to parse flow JSON for {name}: {e}"))
        })?;

        tracing::info!(flow = %flow.id, url = %url, "downloaded remote flow definition");
        Ok(flow)
    }

    /// Download a `.wasm` block from a direct URL.
    #[cfg(feature = "wasm")]
    async fn download_wasm_from_url(
        &mut self,
        client: &reqwest::Client,
        url: &str,
        name: &str,
    ) -> Result<Arc<dyn Block>, RuntimeError> {
        use crate::wasm::{capabilities::BlockCapabilities, WasmiBlock};

        let resp = client
            .get(url)
            .header("User-Agent", "wafer-run/0.1.0")
            .send()
            .await
            .map_err(|e| RuntimeError::Wasm(format!("failed to download WASM for {name}: {e}")))?;

        let status = resp.status().as_u16();
        if status != 200 {
            return Err(RuntimeError::Wasm(format!(
                "failed to download WASM for {name}: HTTP {status}"
            )));
        }

        let body = resp
            .bytes()
            .await
            .map_err(|e| RuntimeError::Wasm(format!("failed to read WASM body for {name}: {e}")))?;

        if body.is_empty() {
            return Err(RuntimeError::Wasm(format!(
                "failed to download WASM for {name}: empty response body"
            )));
        }

        let engine = self.wasm_engine()?.clone();
        let block = WasmiBlock::load_with_engine(&engine, &body, BlockCapabilities::none())
            .map_err(|e| RuntimeError::Wasm(format!("failed to load remote block {name}: {e}")))?;

        Ok(Arc::new(block))
    }

    /// Resolve a remote block via the registry. Returns `Ok(None)` if the block
    /// is not found in the registry.
    #[cfg(feature = "wasm")]
    async fn resolve_remote_block(
        &mut self,
        client: &reqwest::Client,
        name: &str,
    ) -> Result<Option<Arc<dyn Block>>, RuntimeError> {
        let Some(remote_ref) =
            parse_versioned_block(name).or_else(|| parse_unversioned_block(name))
        else {
            return Ok(None);
        };

        let manifest_url = format!(
            "https://raw.githubusercontent.com/wafer-run/registry/main/{}/{}/manifest.json",
            remote_ref.org, remote_ref.block
        );

        let resp = client
            .get(&manifest_url)
            .header("User-Agent", "wafer-run/0.1.0")
            .send()
            .await
            .map_err(|e| {
                RuntimeError::Registry(format!("failed to fetch registry manifest for {name}: {e}"))
            })?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if resp.status().as_u16() != 200 {
            return Err(RuntimeError::Registry(format!(
                "failed to fetch registry manifest for {}: HTTP {}",
                name,
                resp.status().as_u16()
            )));
        }

        let manifest_bytes = resp.bytes().await.map_err(|e| {
            RuntimeError::Registry(format!("failed to read manifest for {name}: {e}"))
        })?;
        let manifest: RegistryManifest = serde_json::from_slice(&manifest_bytes).map_err(|e| {
            RuntimeError::Registry(format!("failed to parse registry manifest for {name}: {e}"))
        })?;

        let version = if remote_ref.version == "latest" {
            manifest.latest.clone()
        } else {
            remote_ref.version.clone()
        };

        let entry = manifest.versions.get(&version).ok_or_else(|| {
            RuntimeError::Registry(format!(
                "version {version} not found in registry for {name}"
            ))
        })?;

        if entry.abi != ABI_VERSION {
            return Err(RuntimeError::AbiMismatch {
                name: name.to_string(),
                required: entry.abi,
                supported: ABI_VERSION,
            });
        }

        if let Some(wasm_url) = &entry.wasm_url {
            let block = self.download_wasm_from_url(client, wasm_url, name).await?;
            Ok(Some(block))
        } else if let Some(flow_url) = &entry.flow_url {
            tracing::debug!(block = %name, flow_url = %flow_url, "block is a flow, not a WASM block");
            Ok(None)
        } else {
            let crate_name = format!("wafer-block-{}", remote_ref.block);
            Err(RuntimeError::Registry(format!(
                "Block \"{name}\" is native-only and must be compiled in.\n\
                 Add it with: cargo add {crate_name}"
            )))
        }
    }

    /// Get or create the shared WASM engine.
    #[cfg(feature = "wasmi")]
    pub fn wasm_engine(&mut self) -> Result<&wasmi::Engine, RuntimeError> {
        if self.wasm_engine.is_none() {
            let mut config = wasmi::Config::default();
            config.consume_fuel(true);
            let engine = wasmi::Engine::new(&config);
            self.wasm_engine = Some(Arc::new(engine));
        }
        Ok(self
            .wasm_engine
            .as_ref()
            .expect("wasm_engine initialized above"))
    }
}

fn log_widening_attempts(
    name: &str,
    overrides: &wafer_block::capabilities::ConfigCapabilityOverrides,
    effective: &wafer_block::BlockCapabilities,
) {
    // Booleans: if config explicitly set `true` but effective is `false`, declared denied.
    for (label, over, eff) in [
        ("raw_sql", overrides.raw_sql, effective.raw_sql),
        ("ddl", overrides.ddl, effective.ddl),
        ("crypto", overrides.crypto, effective.crypto),
        ("network", overrides.network, effective.network),
        ("config", overrides.config, effective.config),
    ] {
        if let Some(true) = over {
            if !eff {
                tracing::warn!(
                    block = %name,
                    field = %label,
                    "config widened capability beyond declared — narrower declaration wins"
                );
            }
        }
    }

    // HashSet allowlists: items in the override that did NOT survive intersection.
    let hash_fields = [
        (
            "collections",
            overrides.collections.as_ref(),
            &effective.collections,
        ),
        (
            "storage_folders",
            overrides.storage_folders.as_ref(),
            &effective.storage_folders,
        ),
        (
            "config_keys",
            overrides.config_keys.as_ref(),
            &effective.config_keys,
        ),
        (
            "callable_blocks",
            overrides.callable_blocks.as_ref(),
            &effective.callable_blocks,
        ),
    ];
    for (label, over_opt, eff_set) in hash_fields {
        if let Some(over_set) = over_opt {
            for item in over_set.iter() {
                if !eff_set.contains(item) {
                    tracing::warn!(
                        block = %name,
                        field = %label,
                        item = %item,
                        "config widened capability beyond declared — narrower declaration wins"
                    );
                }
            }
        }
    }

    // Vec allowlists: same shape.
    let vec_fields = [
        (
            "network_allow",
            overrides.network_allow.as_ref(),
            &effective.network_allow,
        ),
        (
            "headers.readable",
            overrides.headers.as_ref().and_then(|h| h.readable.as_ref()),
            &effective.headers.readable,
        ),
        (
            "headers.writable",
            overrides.headers.as_ref().and_then(|h| h.writable.as_ref()),
            &effective.headers.writable,
        ),
    ];
    for (label, over_opt, eff_vec) in vec_fields {
        if let Some(over_vec) = over_opt {
            for item in over_vec.iter() {
                if !eff_vec.iter().any(|x| x == item) {
                    tracing::warn!(
                        block = %name,
                        field = %label,
                        item = %item,
                        "config widened capability beyond declared — narrower declaration wins"
                    );
                }
            }
        }
    }
}
