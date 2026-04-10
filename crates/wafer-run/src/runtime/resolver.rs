use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::block::Block;
use crate::types::*;

use super::Wafer;
#[cfg(feature = "wasm")]
use super::{parse_unversioned_block, parse_versioned_block, RegistryManifest, ABI_VERSION};

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
        #[allow(clippy::type_complexity)]
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
            let flow_config = match self.block_configs.remove(&flow_id) {
                Some(c) => c,
                None => continue,
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

    /// Resolve walks all flows and resolves block references.
    ///
    /// Before resolving flows, initializes all registered blocks via
    /// `lifecycle(Init)`. Blocks with configs (from `load_blocks_json` or
    /// `add_block_config`) are initialized first (infrastructure), then
    /// remaining blocks are initialized (features that may depend on infra).
    pub async fn resolve(&mut self) -> Result<(), String> {
        // Resolve remote entries: download .flow.json / .wasm for deferred registrations
        #[cfg(feature = "wasm")]
        self.resolve_remote_entries().await?;

        // Expand composite configs (e.g. wafer-run/http-server → http-listener + router)
        self.expand_composite_configs();
        // Expand declarative flow config_map / config_defaults
        self.expand_declarative_flow_configs();
        // Gather uses contributions before initializing blocks
        self.gather_uses_configs();

        // Snapshot expanded configs for inspector before draining
        self.block_configs_snapshot = Arc::new(self.block_configs.clone());

        let configs: Vec<(String, serde_json::Value)> = self.block_configs.drain().collect();

        // Collect names of all pre-registered blocks for phase 2 ordering.
        let pre_registered: Vec<String> = self.blocks.keys().cloned().collect();

        // Track which blocks were initialized with config data.
        let config_names: std::collections::HashSet<String> =
            configs.iter().map(|(n, _)| n.clone()).collect();

        // Sort configs: wafer-run/* infrastructure blocks first, then everything else.
        let mut infra_configs = Vec::new();
        let mut feature_configs = Vec::new();
        for entry in &configs {
            if entry.0.starts_with("wafer-run/") {
                infra_configs.push(entry);
            } else {
                feature_configs.push(entry);
            }
        }

        // Phase 1a: Initialize infrastructure blocks (wafer-run/*) with configs.
        self.rebuild_all_blocks();
        self.collect_wrap_grants();
        for (name, config) in &infra_configs {
            if let Some(block) = self.blocks.get(name.as_str()) {
                let ctx = self.make_context(
                    "init",
                    name.as_str(),
                    HashMap::new(),
                    Arc::new(AtomicBool::new(false)),
                    None,
                );

                let config_data = serde_json::to_vec(config)
                    .map_err(|e| format!("serialize config for block {:?}: {}", name, e))?;
                block
                    .lifecycle(
                        &ctx,
                        LifecycleEvent {
                            event_type: LifecycleType::Init,
                            data: config_data,
                        },
                    )
                    .await
                    .map_err(|e| format!("init block {:?}: {}", name, e))?;
            } else {
                tracing::warn!(block = %name, "block config present but no block registered — skipping");
            }
        }

        // Phase 1b: Initialize feature blocks with configs.
        self.rebuild_all_blocks();
        for (name, config) in &feature_configs {
            if let Some(block) = self.blocks.get(name.as_str()) {
                let ctx = self.make_context(
                    "init",
                    name.as_str(),
                    HashMap::new(),
                    Arc::new(AtomicBool::new(false)),
                    None,
                );

                let config_data = serde_json::to_vec(config)
                    .map_err(|e| format!("serialize config for block {:?}: {}", name, e))?;
                block
                    .lifecycle(
                        &ctx,
                        LifecycleEvent {
                            event_type: LifecycleType::Init,
                            data: config_data,
                        },
                    )
                    .await
                    .map_err(|e| format!("init block {:?}: {}", name, e))?;
            } else {
                tracing::warn!(block = %name, "block config present but no block registered — skipping");
            }
        }

        // Rebuild the all_blocks snapshot so lifecycle contexts can find
        // all blocks during phase 2.
        self.rebuild_all_blocks();

        // Phase 2: Initialize remaining pre-registered blocks (no config).
        for name in &pre_registered {
            if config_names.contains(name) {
                continue; // Already initialized in phase 1
            }
            if let Some(block) = self.blocks.get(name) {
                let ctx = self.make_context(
                    "init",
                    name.as_str(),
                    HashMap::new(),
                    Arc::new(AtomicBool::new(false)),
                    None,
                );
                block
                    .lifecycle(
                        &ctx,
                        LifecycleEvent {
                            event_type: LifecycleType::Init,
                            data: Vec::new(),
                        },
                    )
                    .await
                    .map_err(|e| format!("init block {:?}: {}", name, e))?;
            }
        }

        // Phase 3: Verify flow block references exist.
        // Collect all referenced block names first to avoid borrow conflict.
        let referenced_blocks: Vec<String> = self
            .flows
            .values()
            .flat_map(|f| f.steps.iter())
            .map(|step| {
                self.aliases
                    .get(&step.block)
                    .cloned()
                    .unwrap_or_else(|| step.block.clone())
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        for block_name in referenced_blocks {
            if self.blocks.contains_key(&block_name) {
                continue;
            }
            // Try WASM download
            #[cfg(feature = "wasm")]
            {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .map_err(|e| format!("failed to create HTTP client: {}", e))?;
                match self.resolve_remote_block(&client, &block_name).await? {
                    Some(block) => {
                        tracing::info!(block = %block_name, "downloaded remote block");
                        let ctx = self.make_context(
                            "init",
                            block_name.as_str(),
                            HashMap::new(),
                            Arc::new(AtomicBool::new(false)),
                            None,
                        );
                        block
                            .lifecycle(
                                &ctx,
                                LifecycleEvent {
                                    event_type: LifecycleType::Init,
                                    data: Vec::new(),
                                },
                            )
                            .await
                            .map_err(|e| format!("init remote block {:?}: {}", block_name, e))?;
                        self.blocks.insert(block_name.clone(), block);
                    }
                    None => {
                        return Err(format!("block type not found: {}", block_name));
                    }
                }
            }
            #[cfg(not(feature = "wasm"))]
            return Err(format!("block type not found: {}", block_name));
        }

        // Rebuild snapshot so any Phase-3-resolved blocks are visible.
        self.rebuild_all_blocks();

        Ok(())
    }

    /// Resolve remote blocks for deferred registrations via the registry.
    #[cfg(feature = "wasm")]
    async fn resolve_remote_entries(&mut self) -> Result<(), String> {
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
            .map_err(|e| format!("failed to create HTTP client: {}", e))?;

        for name in candidates {
            let remote_ref =
                parse_versioned_block(&name).or_else(|| parse_unversioned_block(&name));
            let remote_ref = match remote_ref {
                Some(r) => r,
                None => continue,
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
                .map_err(|e| format!("failed to fetch registry manifest for {}: {}", name, e))?;

            if resp.status().as_u16() == 404 {
                continue;
            }
            if resp.status().as_u16() != 200 {
                return Err(format!(
                    "failed to fetch registry manifest for {}: HTTP {}",
                    name,
                    resp.status().as_u16()
                ));
            }

            let manifest_bytes = resp
                .bytes()
                .await
                .map_err(|e| format!("failed to read manifest for {}: {}", name, e))?;
            let manifest: RegistryManifest = serde_json::from_slice(&manifest_bytes)
                .map_err(|e| format!("failed to parse registry manifest for {}: {}", name, e))?;

            let version = if remote_ref.version == "latest" {
                manifest.latest.clone()
            } else {
                remote_ref.version.clone()
            };

            let entry = manifest
                .versions
                .get(&version)
                .ok_or_else(|| format!("version {} not found in registry for {}", version, name))?;

            if entry.abi != ABI_VERSION {
                return Err(format!(
                    "block {} version {} requires ABI {} but runtime supports ABI {}",
                    name, version, entry.abi, ABI_VERSION
                ));
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
                            self.blocks.insert(block_name.clone(), block);
                        }
                        Ok(None) => {
                            tracing::debug!(
                                block = %block_name,
                                "block not found in registry, will resolve during step resolution"
                            );
                        }
                        Err(e) => {
                            return Err(format!(
                                "failed to download block dependency {:?} for flow {:?}: {}",
                                block_name, name, e
                            ));
                        }
                    }
                }
            } else if let Some(wasm_url) = &entry.wasm_url {
                let block = self
                    .download_wasm_from_url(&client, wasm_url, &name)
                    .await?;
                tracing::info!(block = %name, "downloaded remote WASM block from registry");
                self.blocks.insert(name.clone(), block);
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
    ) -> Result<wafer_flow::WaferFlow, String> {
        let resp = client
            .get(url)
            .header("User-Agent", "wafer-run/0.1.0")
            .send()
            .await
            .map_err(|e| format!("failed to download flow for {}: {}", name, e))?;

        if resp.status().as_u16() != 200 {
            return Err(format!(
                "failed to download flow for {}: HTTP {}",
                name,
                resp.status().as_u16()
            ));
        }

        let body = resp
            .bytes()
            .await
            .map_err(|e| format!("failed to read flow body for {}: {}", name, e))?;

        let body_str = std::str::from_utf8(&body)
            .map_err(|e| format!("failed to decode flow body for {}: {}", name, e))?;

        let flow = wafer_flow::parse(body_str)
            .map_err(|e| format!("failed to parse flow JSON for {}: {}", name, e))?;

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
    ) -> Result<Arc<dyn Block>, String> {
        use crate::wasm::capabilities::BlockCapabilities;
        use crate::wasm::WasmiBlock;

        let resp = client
            .get(url)
            .header("User-Agent", "wafer-run/0.1.0")
            .send()
            .await
            .map_err(|e| format!("failed to download WASM for {}: {}", name, e))?;

        let status = resp.status().as_u16();
        if status != 200 {
            return Err(format!(
                "failed to download WASM for {}: HTTP {}",
                name, status
            ));
        }

        let body = resp
            .bytes()
            .await
            .map_err(|e| format!("failed to read WASM body for {}: {}", name, e))?;

        if body.is_empty() {
            return Err(format!(
                "failed to download WASM for {}: empty response body",
                name
            ));
        }

        let engine = self.wasm_engine()?.clone();
        let block = WasmiBlock::load_with_engine(&engine, &body, BlockCapabilities::none())
            .map_err(|e| format!("failed to load remote block {}: {}", name, e))?;

        Ok(Arc::new(block))
    }

    /// Resolve a remote block via the registry. Returns `Ok(None)` if the block
    /// is not found in the registry.
    #[cfg(feature = "wasm")]
    async fn resolve_remote_block(
        &mut self,
        client: &reqwest::Client,
        name: &str,
    ) -> Result<Option<Arc<dyn Block>>, String> {
        let remote_ref = parse_versioned_block(name).or_else(|| parse_unversioned_block(name));
        let remote_ref = match remote_ref {
            Some(r) => r,
            None => return Ok(None),
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
            .map_err(|e| format!("failed to fetch registry manifest for {}: {}", name, e))?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if resp.status().as_u16() != 200 {
            return Err(format!(
                "failed to fetch registry manifest for {}: HTTP {}",
                name,
                resp.status().as_u16()
            ));
        }

        let manifest_bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("failed to read manifest for {}: {}", name, e))?;
        let manifest: RegistryManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| format!("failed to parse registry manifest for {}: {}", name, e))?;

        let version = if remote_ref.version == "latest" {
            manifest.latest.clone()
        } else {
            remote_ref.version.clone()
        };

        let entry = manifest
            .versions
            .get(&version)
            .ok_or_else(|| format!("version {} not found in registry for {}", version, name))?;

        if entry.abi != ABI_VERSION {
            return Err(format!(
                "block {} version {} requires ABI {} but runtime supports ABI {}",
                name, version, entry.abi, ABI_VERSION
            ));
        }

        if let Some(wasm_url) = &entry.wasm_url {
            let block = self.download_wasm_from_url(client, wasm_url, name).await?;
            Ok(Some(block))
        } else if let Some(flow_url) = &entry.flow_url {
            tracing::debug!(block = %name, flow_url = %flow_url, "block is a flow, not a WASM block");
            Ok(None)
        } else {
            let crate_name = format!("wafer-block-{}", remote_ref.block);
            Err(format!(
                "Block \"{}\" is native-only and must be compiled in.\n\
                 Add it with: cargo add {}",
                name, crate_name
            ))
        }
    }

    /// Get or create the shared WASM engine.
    #[cfg(feature = "wasmi")]
    pub fn wasm_engine(&mut self) -> Result<&wasmi::Engine, String> {
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
