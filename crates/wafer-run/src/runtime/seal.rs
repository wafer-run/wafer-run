//! [`Wafer::seal`] — the once-per-boot finalization pipeline, decomposed
//! into named phases: grant-rejection gate, remote resolution, config
//! expansion, capability computation, wasm instance-pooling policy,
//! block-reference resolution, and startup-snapshot finalization.

use std::{collections::BTreeMap, sync::Arc};

use wafer_block::error::{BlockReferenceError, BlockReferenceSource, RuntimeError};

use super::Wafer;

impl Wafer {
    /// Finalize runtime configuration before serving traffic.
    ///
    /// `seal()` performs the once-per-boot operations that lazy init does
    /// **not** subsume:
    ///
    /// 1. Refuse boot if any WRAP grants were rejected during registration.
    /// 2. Resolve remote entries (download `.flow.json` / `.wasm` for
    ///    deferred registrations).
    /// 3. Expand composite configs (e.g. `wafer-run/http-server` →
    ///    `http-listener` + `router`), declarative flow `config_map` /
    ///    `config_defaults`, and `"uses"` contributions across all block
    ///    configs.
    /// 4. Compute effective capabilities per block (declared ∩ config ∩ host)
    ///    and propagate them into each block. Then resolve the wasm
    ///    instance-pooling policy: validate the host kill switch
    ///    (`WAFER_RUN_WASM_POOLING` — invalid values refuse boot) and log
    ///    each WASM block whose declared `Singleton`/`PerFlow` instance mode
    ///    opts it into warm instance pooling (PERF-01).
    /// 5. Resolve remote blocks referenced by flow steps and router routes.
    ///    Aggregates every missing reference into one
    ///    `RuntimeError::BlocksNotFound` so operators see the full punch
    ///    list with each missing block's source.
    /// 6. Finalize the [`crate::snapshot::StartupSnapshot`] consumed by
    ///    every [`crate::runtime::RuntimeContext`].
    ///
    /// Block `Init` lifecycle events are **not** dispatched here. Each block
    /// is initialized on first dispatch via [`Wafer::init_block`] (lazy
    /// once-success). Required-config presence is **not** validated here —
    /// broken paths surface as 5xx on first invocation. Use
    /// [`Wafer::validate_all_block_configs`] for proactive health checks.
    pub async fn seal(&mut self) -> Result<(), RuntimeError> {
        self.fail_on_rejected_grants()?;

        #[cfg(feature = "wasm")]
        self.resolve_remote_entries().await?;

        self.expand_composite_configs();
        self.expand_declarative_flow_configs();
        self.gather_uses_configs();

        self.compute_effective_capabilities()?;

        #[cfg(feature = "wasmi")]
        self.log_wasm_instance_pooling()?;

        self.resolve_block_references().await?;

        self.finalize_snapshot();
        Ok(())
    }

    /// Drain the grant-validation accumulator before sealing. This is the
    /// common boot funnel: both `start_with_priority()` (native) and direct
    /// `seal()` callers (Cloudflare Workers, browser WASM) pass through here.
    /// If any typed grants were rejected during register_block /
    /// set_admin_block, refuse boot with all rejections listed in one error.
    fn fail_on_rejected_grants(&mut self) -> Result<(), RuntimeError> {
        if self.registration.wrap.validation_errors.is_empty() {
            return Ok(());
        }
        let errors = std::mem::take(&mut self.registration.wrap.validation_errors);
        Err(RuntimeError::GrantsRejected(errors))
    }

    /// Compute effective capabilities per block: declared ∩ config ∩ host.
    /// Also strips the reserved `capabilities` subkey from each block config
    /// so it doesn't leak into `ctx.config_get(...)`.
    ///
    /// Refuses boot (`RuntimeError::Config`) if any block's `capabilities`
    /// subkey fails to parse — a mistyped override must never silently fall
    /// back to the block's declared (often unrestricted) capabilities.
    fn compute_effective_capabilities(&mut self) -> Result<(), RuntimeError> {
        let mut eff: std::collections::HashMap<String, wafer_block::BlockCapabilities> =
            std::collections::HashMap::new();
        for (name, block) in &self.registration.blocks {
            let info = block.info();
            let declared = match info.capabilities {
                Some(caps) => caps,
                // SEC-02: an omitted capability declaration must default by
                // runtime type, not unconditionally to `unrestricted()`. A
                // WASM guest is untrusted — fail closed to `none()` so a
                // missing declaration cannot silently grant crypto, network,
                // raw SQL, DDL, config, storage, vector, and `call_block` to
                // every registered block. Native blocks are compiled into the
                // host binary and trusted, so they retain the historical
                // unrestricted default.
                None => match info.runtime {
                    wafer_block::BlockRuntime::Wasm => wafer_block::BlockCapabilities::none(),
                    wafer_block::BlockRuntime::Native => {
                        wafer_block::BlockCapabilities::unrestricted()
                    }
                },
            };

            let config_overrides =
                take_capability_overrides(name, self.registration.block_configs.get_mut(name))?;

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
        self.registration.wrap.effective_capabilities = Arc::new(eff);
        Ok(())
    }

    /// Resolve + log the wasm instance-pooling policy (PERF-01, COR-02).
    ///
    /// A WASM block declaring a state-retaining `InstanceMode` (`Singleton`,
    /// `PerFlow`) opts into `WasmiBlock`'s warm instance pool: its store +
    /// instance are reused across `handle` calls, so guest globals and heap
    /// survive between calls. (This retired the pre-pooling seal warning
    /// that such declarations were advisory and unhonorable on the wasm
    /// path.) Validates the `WAFER_RUN_WASM_POOLING` kill switch here too so
    /// a mistyped value refuses boot even before any wasm block loads, then
    /// logs the effective decision per opted-in block. Runs after
    /// `resolve_remote_entries` so downloaded WASM blocks are covered too.
    #[cfg(feature = "wasmi")]
    fn log_wasm_instance_pooling(&self) -> Result<(), RuntimeError> {
        let pooling_enabled = crate::wasm::wasmi_loader::wasm_pooling_host_override()?;
        self.log_pooling_decisions(pooling_enabled);
        Ok(())
    }

    /// Log, per WASM block that declared a state-retaining instance mode,
    /// whether the host lets it pool. Split from
    /// [`log_wasm_instance_pooling`](Self::log_wasm_instance_pooling) so
    /// tests can drive both branches without mutating process-global env.
    #[cfg(feature = "wasmi")]
    fn log_pooling_decisions(&self, pooling_enabled: bool) {
        for (name, block) in &self.registration.blocks {
            let info = block.info();
            if info.runtime == wafer_block::BlockRuntime::Wasm
                && matches!(
                    info.instance_mode,
                    wafer_block::InstanceMode::Singleton | wafer_block::InstanceMode::PerFlow
                )
            {
                if pooling_enabled {
                    tracing::info!(
                        block = %name,
                        declared = %info.instance_mode,
                        "declared instance_mode opts this WASM block into warm instance \
                         pooling: instances are reused across calls, so guest state may \
                         survive between invocations"
                    );
                } else {
                    tracing::warn!(
                        block = %name,
                        declared = %info.instance_mode,
                        "declared instance_mode is not honored: wasm instance pooling is \
                         disabled by WAFER_RUN_WASM_POOLING=off, so this block runs as a \
                         fresh instance per call and retains no guest state"
                    );
                }
            }
        }
    }

    /// Resolve remote blocks referenced by flow steps and router routes.
    /// Collect every reference + its source, then resolve-or-aggregate-fail.
    ///
    /// PR A landed the flow-step half of the walk. PR B (Wave 16) extended
    /// the collection to also include router routes; Wave 19 routed that
    /// half through `Block::collect_block_refs` so every block type can
    /// declare its own config-held references via the trait.
    async fn resolve_block_references(&mut self) -> Result<(), RuntimeError> {
        // BTreeMap (vs HashMap) so iteration over missing references yields
        // canonical-name-sorted order, giving stable `Display` output for
        // `BlocksNotFound` across boots.
        let mut references: BTreeMap<String, Vec<BlockReferenceSource>> = BTreeMap::new();

        for (flow_id, flow) in &self.flows {
            collect_flow_step_refs(self, flow_id, &flow.steps, &[], &mut references);
        }
        // Walk every block-config entry; ask the registered block what
        // references its config holds. Default impl on `Block` returns
        // empty Vec, so only blocks that override (router today; future
        // composite/middleware blocks) emit anything.
        for (block_name, config) in &self.registration.block_configs {
            let canonical_block = self.canonicalize(block_name).to_string();
            let Some(block) = self.registration.blocks.get(&canonical_block) else {
                // Block config exists but the block isn't registered yet
                // (will be downloaded from registry below, or flagged as
                // not_found). Skip — we can't ask an unregistered block
                // to walk its own config.
                continue;
            };
            for r in block.collect_block_refs(config) {
                let canonical = self.canonicalize(&r.target).to_string();
                references
                    .entry(canonical)
                    .or_default()
                    .push(BlockReferenceSource::BlockConfig {
                        from_block: block_name.clone(),
                        location: r.location,
                        detail: r.detail,
                    });
            }
        }

        let mut not_found: Vec<BlockReferenceError> = Vec::new();

        // Build the HTTP client once per seal() rather than per missing
        // block — aligns with `resolve_remote_entries`, which already
        // hoists. The client is short-lived (one seal() call) and reused
        // across every remote-block resolution attempt in the loop.
        #[cfg(feature = "wasm")]
        let client = Self::registry_http_client()?;

        for (canonical, sources) in references {
            if self.registration.blocks.contains_key(&canonical) {
                continue;
            }
            #[cfg(feature = "wasm")]
            {
                match self.resolve_remote_block(&client, &canonical).await {
                    Ok(Some(block)) => {
                        tracing::info!(block = %canonical, "downloaded remote block");
                        self.registration.register_remote_block(&canonical, block)?;
                        continue;
                    }
                    Ok(None) => {
                        // Fall through to the not_found push below.
                    }
                    Err(e) => {
                        // Don't abort the aggregator on a flaky registry response;
                        // log + treat the entry as not_found so operators still see
                        // the full punch list of missing references with their
                        // original sources.
                        tracing::warn!(
                            block = %canonical,
                            error = %e,
                            "registry resolution failed during seal; treating as not_found"
                        );
                    }
                }
            }
            not_found.push(BlockReferenceError {
                name: canonical,
                sources,
            });
        }

        if not_found.is_empty() {
            Ok(())
        } else {
            Err(RuntimeError::BlocksNotFound(not_found))
        }
    }

    /// Finalize the startup snapshot. Block configs survive in
    /// `self.registration.block_configs` and are mirrored here for context
    /// consumers. (Lazy init reads from the runtime's `ConfigSource` — not
    /// from `self.registration.block_configs` — when dispatching
    /// `lifecycle(Init)` on first request; see `run_init_pipeline`.)
    ///
    /// Also compiles the [`SealedPlan`](crate::runtime::exec_plan::SealedPlan)
    /// from the snapshot (PERF-03): parsed block configs, `requires`
    /// allowlists, and compiled flows for the dispatch hot paths.
    fn finalize_snapshot(&mut self) {
        self.rebuild_all_blocks();
        self.snapshot = Arc::new(crate::snapshot::StartupSnapshot {
            blocks: super::lifecycle::sorted_snapshot(
                self.registration.blocks.values().map(|b| b.info()),
            ),
            flow_infos: self.flows_info(),
            flow_defs: self.flow_defs(),
            block_configs: self.registration.block_configs.clone(),
            interface_specs: self
                .registration
                .interface_specs
                .values()
                .cloned()
                .collect(),
        });
        self.plan = self.compile_plan();
    }
}

/// Strip the reserved `capabilities` subkey from `cfg` (when present) and
/// parse it into `ConfigCapabilityOverrides`. Returns defaults when there is
/// no config for the block, the config isn't an object, or the subkey is
/// absent. Refuses (`RuntimeError::Config`) when the subkey is present but
/// fails to parse — a mistyped override (e.g. a string where a bool is
/// expected) must fail closed rather than silently drop to "no override",
/// which would leave the block at its declared (often unrestricted)
/// capabilities. Matches the `fail_on_rejected_grants` precedent.
fn take_capability_overrides(
    name: &str,
    cfg: Option<&mut serde_json::Value>,
) -> Result<wafer_block::capabilities::ConfigCapabilityOverrides, RuntimeError> {
    let Some(raw) = cfg
        .and_then(|c| c.as_object_mut())
        .and_then(|obj| obj.remove("capabilities"))
    else {
        return Ok(wafer_block::capabilities::ConfigCapabilityOverrides::default());
    };
    serde_json::from_value(raw).map_err(|e| {
        RuntimeError::Config(format!("block {name}: invalid `capabilities` config: {e}"))
    })
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

/// Walks a slice of `wafer_flow::Step`s and emits one entry per
/// (canonical_block_name, BlockReferenceSource::Flow) into
/// `references`. Recurses through `step.parallel[].steps[]` with
/// `parallel_path` accumulating (outer_step_index, branch_index)
/// pairs along the way.
///
/// `seal()` calls this once per flow with `parallel_path = &[]`.
fn collect_flow_step_refs(
    wafer: &Wafer,
    flow_id: &str,
    steps: &[wafer_flow::Step],
    parallel_path: &[(usize, usize)],
    references: &mut BTreeMap<String, Vec<BlockReferenceSource>>,
) {
    for (step_index, step) in steps.iter().enumerate() {
        let canonical = wafer.canonicalize(&step.block).to_string();
        references
            .entry(canonical)
            .or_default()
            .push(BlockReferenceSource::Flow {
                flow_id: flow_id.to_string(),
                step_index,
                step_id: step.id.clone(),
                parallel_path: if parallel_path.is_empty() {
                    None
                } else {
                    Some(parallel_path.to_vec())
                },
            });
        if let Some(branches) = &step.parallel {
            for (branch_index, branch) in branches.iter().enumerate() {
                let mut nested = parallel_path.to_vec();
                nested.push((step_index, branch_index));
                collect_flow_step_refs(wafer, flow_id, &branch.steps, &nested, references);
            }
        }
    }
}

#[cfg(all(test, feature = "wasmi"))]
mod instance_mode_tests {
    use std::sync::Arc;

    use wafer_block::{
        streams::{input::InputStream, output::OutputStream},
        types::BlockRuntime,
        Block, BlockInfo, Context, InstanceMode, LifecycleEvent, Message, WaferError,
    };

    use super::super::Wafer;

    /// Minimal block whose `info()` is whatever the test says it is —
    /// lets us fabricate `runtime = Wasm` without loading real wasm.
    struct FakeInfoBlock {
        info: BlockInfo,
    }

    #[async_trait::async_trait]
    impl Block for FakeInfoBlock {
        fn info(&self) -> BlockInfo {
            self.info.clone()
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::respond(Vec::new())
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _event: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
    }

    fn register(wafer: &mut Wafer, info: BlockInfo) {
        let name = info.name.clone();
        wafer
            .register_block(name, Arc::new(FakeInfoBlock { info }))
            .expect("register");
    }

    /// COR-02 / PERF-01: a WASM block declaring a state-retaining mode
    /// (`Singleton`, `PerFlow`) opts into warm instance pooling — the seal
    /// phase logs that reuse is in effect (guest state may survive across
    /// calls) instead of the retired "advisory, unhonorable" warning.
    #[test]
    #[tracing_test::traced_test]
    fn wasm_state_retaining_instance_mode_logs_pooling() {
        let mut wafer = Wafer::empty();
        register(
            &mut wafer,
            BlockInfo::new("test/wasm-singleton", "0.1.0", "middleware@v1", "wasm")
                .runtime(BlockRuntime::Wasm)
                .instance_mode(InstanceMode::Singleton),
        );
        register(
            &mut wafer,
            BlockInfo::new("test/wasm-per-flow", "0.1.0", "middleware@v1", "wasm")
                .runtime(BlockRuntime::Wasm)
                .instance_mode(InstanceMode::PerFlow),
        );

        wafer.log_pooling_decisions(true);

        logs_assert(|lines: &[&str]| {
            let n = lines
                .iter()
                .filter(|l| l.contains("warm instance pooling"))
                .count();
            if n == 2 {
                Ok(())
            } else {
                Err(format!("expected 2 pooling logs, got {n}"))
            }
        });
    }

    /// With the host kill switch off (`WAFER_RUN_WASM_POOLING=off`), a
    /// declared state-retaining mode is back to unhonorable — the block runs
    /// cold — and the seal phase must warn so the author doesn't discover it
    /// as silently-lost guest state.
    #[test]
    #[tracing_test::traced_test]
    fn kill_switch_off_warns_declared_mode_not_honored() {
        let mut wafer = Wafer::empty();
        register(
            &mut wafer,
            BlockInfo::new("test/wasm-singleton", "0.1.0", "middleware@v1", "wasm")
                .runtime(BlockRuntime::Wasm)
                .instance_mode(InstanceMode::Singleton),
        );

        wafer.log_pooling_decisions(false);

        logs_assert(|lines: &[&str]| {
            let n = lines
                .iter()
                .filter(|l| l.contains("instance_mode is not honored"))
                .count();
            if n == 1 {
                Ok(())
            } else {
                Err(format!("expected 1 not-honored warning, got {n}"))
            }
        });
    }

    /// The pooling phase must stay quiet for modes that don't opt in: WASM +
    /// `PerExecution`/`PerNode` keep per-call instantiation, and a native
    /// block's shared instance honors `Singleton` natively. Guards against
    /// boot-log noise.
    #[test]
    #[tracing_test::traced_test]
    fn non_opted_instance_modes_stay_quiet() {
        let mut wafer = Wafer::empty();
        register(
            &mut wafer,
            BlockInfo::new("test/wasm-per-exec", "0.1.0", "middleware@v1", "wasm")
                .runtime(BlockRuntime::Wasm)
                .instance_mode(InstanceMode::PerExecution),
        );
        register(
            &mut wafer,
            BlockInfo::new("test/native-singleton", "0.1.0", "middleware@v1", "native")
                .instance_mode(InstanceMode::Singleton),
        );

        wafer.log_pooling_decisions(true);
        wafer.log_pooling_decisions(false);

        logs_assert(|lines: &[&str]| {
            let n = lines
                .iter()
                .filter(|l| {
                    l.contains("warm instance pooling") || l.contains("instance_mode is not")
                })
                .count();
            if n == 0 {
                Ok(())
            } else {
                Err(format!("expected no pooling logs, got {n}"))
            }
        });
    }
}
