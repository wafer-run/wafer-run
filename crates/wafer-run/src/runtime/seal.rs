//! [`Wafer::seal`] — the once-per-boot finalization pipeline, decomposed
//! into named phases: grant-rejection gate, remote resolution, config
//! expansion, capability computation, wasm instance-pooling policy,
//! block-reference resolution, agent-tool-name uniqueness, and
//! startup-snapshot finalization.

use std::{collections::BTreeMap, sync::Arc};

use wafer_block::error::{
    AgentToolClaimant, BlockReferenceError, BlockReferenceSource, DuplicateToolNameError,
    RuntimeError,
};

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
    /// 6. Refuse boot if two endpoints — in the same block or across blocks
    ///    — declare the same WebMCP agent-tool name. Runs after step 5 so
    ///    downloaded remote blocks are covered too.
    /// 7. Finalize the [`crate::snapshot::StartupSnapshot`] consumed by
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

        self.fail_on_duplicate_tool_names()?;

        self.finalize_snapshot();
        Ok(())
    }

    /// Refuse boot when two endpoints declare the same WebMCP agent-tool
    /// name.
    ///
    /// # Why this is a deployment-time failure and not a runtime one
    ///
    /// `BlockInfo::validate` already rejects a *malformed* tool name at
    /// registration, but it sees one block at a time, so a name claimed by
    /// two blocks — or by two endpoints of one block — reaches the runtime
    /// intact. The manifest projection then has to decide what to do with an
    /// ambiguous name, and every available runtime answer is bad:
    ///
    /// * Drop both claimants globally, and the *absence* of a name in an
    ///   anonymous caller's manifest tells them an endpoint they cannot
    ///   reach claims it — an existence oracle.
    /// * Drop them per manifest, and the collision resolves differently per
    ///   caller: a `Public` endpoint colliding with an `Admin` one is
    ///   published to anonymous and authenticated callers as the sole
    ///   claimant of the name, and only an admin's manifest reports the
    ///   collision. Every agent below admin binds the name to the shadowing
    ///   endpoint with no diagnostic reaching it.
    ///
    /// Neither is a property the deployment should have. So the ambiguity is
    /// made impossible to declare instead: one loud error at boot, listing
    /// every colliding name and every endpoint claiming it, the same shape
    /// [`Self::fail_on_rejected_grants`] and
    /// [`Self::resolve_block_references`] use for their own cross-block
    /// verdicts. `wafer_core::discovery`'s per-manifest census stays where it
    /// is as a safety net for consumers that project `BlockInfo`s this gate
    /// never saw, and in a runtime that went through `seal()` it never fires.
    ///
    /// Names that are not legal MCP tool names are skipped: registration
    /// already refused them (`BlockInfoError::InvalidAgentToolName`), and
    /// counting them here would let one defect manufacture another — two
    /// endpoints that both forgot a name would collide in the empty-string
    /// bucket and be reported as a duplicate, which says nothing true about
    /// either.
    fn fail_on_duplicate_tool_names(&self) -> Result<(), RuntimeError> {
        // BTreeMap so the aggregated message is name-sorted and stable
        // across boots; `registration.blocks` is a HashMap.
        let mut claimants: BTreeMap<String, Vec<AgentToolClaimant>> = BTreeMap::new();
        for block in self.registration.blocks.values() {
            let info = block.info();
            for ep in &info.endpoints {
                let Some(tool) = ep.agent_tool.as_ref() else {
                    continue;
                };
                if !wafer_block::AgentTool::is_valid_name(&tool.name) {
                    continue;
                }
                claimants
                    .entry(tool.name.clone())
                    .or_default()
                    .push(AgentToolClaimant {
                        block: info.name.clone(),
                        method: ep.method,
                        path: ep.path.clone(),
                    });
            }
        }

        let duplicates: Vec<DuplicateToolNameError> = claimants
            .into_iter()
            .filter(|(_, claimants)| claimants.len() > 1)
            .map(|(name, mut claimants)| {
                claimants.sort_by(|a, b| {
                    (&a.block, a.method.to_string(), &a.path).cmp(&(
                        &b.block,
                        b.method.to_string(),
                        &b.path,
                    ))
                });
                DuplicateToolNameError { name, claimants }
            })
            .collect();

        if duplicates.is_empty() {
            Ok(())
        } else {
            Err(RuntimeError::DuplicateToolNames(duplicates))
        }
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
        ("schema", overrides.schema, effective.schema),
        ("crypto", overrides.crypto, effective.crypto),
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

    // Allowlist capabilities: an override widens when it permits something the
    // effective (declared ∩ override) does not. All six allowlist fields share
    // one loop now (collections/storage_folders/vector_indexes/callable_blocks
    // joined network/config here when they became `Allowlist` — this also
    // closes the pre-existing gap where `vector_indexes` widening was never
    // logged).
    for (label, over_opt, eff) in [
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
        ("network", overrides.network.as_ref(), &effective.network),
        ("config", overrides.config.as_ref(), &effective.config),
        (
            "vector_indexes",
            overrides.vector_indexes.as_ref(),
            &effective.vector_indexes,
        ),
        (
            "callable_blocks",
            overrides.callable_blocks.as_ref(),
            &effective.callable_blocks,
        ),
    ] {
        if let Some(over) = over_opt {
            log_allowlist_widening(name, label, over, eff);
        }
    }

    // Vec allowlists: same shape.
    let vec_fields = [
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

/// Warn when an [`Allowlist`](wafer_block::capabilities::Allowlist) override
/// (SEC-06) permits more than the effective (declared ∩ override) allows —
/// the declaration is narrower and wins, so the operator's extra grant is
/// inert. `None` overrides never widen (deny cannot widen).
fn log_allowlist_widening(
    name: &str,
    label: &str,
    over: &wafer_block::capabilities::Allowlist,
    eff: &wafer_block::capabilities::Allowlist,
) {
    for item in widened_allowlist_entries(over, eff) {
        tracing::warn!(
            block = %name,
            field = %label,
            item = %item,
            "config widened capability beyond declared — narrower declaration wins"
        );
    }
}

/// The override entries that did NOT survive into `eff`, i.e. the ones the
/// operator asked for and the declaration refused. `"*"` stands for an `Any`
/// override that a narrower declaration cut down.
///
/// # Why one comparison serves both matching modes
///
/// This logger is GENERIC over the allowlist fields, and `storage_folders`
/// narrows by path prefix
/// ([`Allowlist::intersect_path_prefix`](wafer_block::capabilities::Allowlist::intersect_path_prefix))
/// rather than by set intersection — but the membership test below stays
/// correct for it, so no prefix-aware variant is needed. Under the prefix
/// intersection an override entry `x` survives VERBATIM whenever some declared
/// entry covers it (`site` declared, `site/jhg` overridden → `site/jhg`
/// survives). It fails to survive in exactly two cases: `x` is broader than a
/// declared entry (only the declared, narrower entry survives) or `x` is
/// unrelated to every declared entry — and both of those ARE widenings worth
/// warning about. So "not present in `eff`" means the same thing under either
/// mode. `storage_folder_override_nested_under_declared_does_not_warn` pins
/// the case that would regress if the intersection ever went back to an exact
/// set operation.
fn widened_allowlist_entries(
    over: &wafer_block::capabilities::Allowlist,
    eff: &wafer_block::capabilities::Allowlist,
) -> Vec<String> {
    use wafer_block::capabilities::Allowlist::{Any, None, Only};
    match (over, eff) {
        // Override asked for everything but the declaration is narrower.
        (Any, Any) => Vec::new(),
        (Any, _) => vec!["*".to_string()],
        // Override listed specifics; any not surviving the intersection widened.
        (Only(over_set), Only(eff_set)) => over_set.difference(eff_set).cloned().collect(),
        (Only(over_set), None) => over_set.iter().cloned().collect(),
        // Effective is Any (≥ override), or override is None → no widening.
        (Only(_), Any) | (None, _) => Vec::new(),
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

#[cfg(test)]
mod widening_tests {
    //! `widened_allowlist_entries` is the pure core of the widening warning.
    //! `storage_folders` narrows by path prefix while every other allowlist
    //! narrows by set intersection, so these pin that ONE comparison stays
    //! honest for both.

    use std::collections::BTreeSet;

    use wafer_block::capabilities::Allowlist;

    use super::widened_allowlist_entries;

    fn only(items: &[&str]) -> Allowlist {
        Allowlist::Only(items.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>())
    }

    /// The M4 case: the operator narrowed a declared folder to a subfolder.
    /// The override entry survives the prefix intersection verbatim, so
    /// nothing widened and nothing may be warned about.
    #[test]
    fn storage_folder_override_nested_under_declared_does_not_warn() {
        let declared = only(&["site"]);
        let over = only(&["site/jhg"]);
        let eff = declared.intersect_path_prefix(&over);
        assert_eq!(eff, only(&["site/jhg"]));
        assert!(
            widened_allowlist_entries(&over, &eff).is_empty(),
            "narrowing to a subfolder is not a widening"
        );
    }

    /// The reverse nesting IS a widening: the operator asked for the parent
    /// folder and only the block's narrower declaration survived.
    #[test]
    fn storage_folder_override_broader_than_declared_warns() {
        let declared = only(&["site/jhg"]);
        let over = only(&["site"]);
        let eff = declared.intersect_path_prefix(&over);
        assert_eq!(eff, only(&["site/jhg"]));
        assert_eq!(widened_allowlist_entries(&over, &eff), vec!["site"]);
    }

    #[test]
    fn unrelated_override_entries_warn() {
        let declared = only(&["site/jhg"]);
        let over = only(&["site/jhg/sub", "other"]);
        let eff = declared.intersect_path_prefix(&over);
        assert_eq!(
            widened_allowlist_entries(&over, &eff),
            vec!["other"],
            "only the entry with no covering relationship widened"
        );
    }

    #[test]
    fn any_override_against_a_narrower_declaration_warns_once() {
        let eff = only(&["users"]);
        assert_eq!(widened_allowlist_entries(&Allowlist::Any, &eff), vec!["*"]);
        assert!(widened_allowlist_entries(&Allowlist::Any, &Allowlist::Any).is_empty());
        assert!(widened_allowlist_entries(&Allowlist::None, &Allowlist::None).is_empty());
        assert!(widened_allowlist_entries(&only(&["users"]), &Allowlist::Any).is_empty());
    }

    #[test]
    fn every_entry_warns_when_the_declaration_denied_the_field() {
        assert_eq!(
            widened_allowlist_entries(&only(&["a", "b"]), &Allowlist::None),
            vec!["a", "b"]
        );
    }

    /// The boolean half of `log_widening_attempts` (`raw_sql`/`ddl`/`schema`/
    /// `crypto`) warns whenever an operator override sets a field `true` but
    /// the block's declaration keeps the effective capability `false` — one
    /// warning line per field. Exercises the loop directly so a field
    /// dropped from it (as `schema` once was) shows up as a test failure
    /// rather than a silent logging gap.
    #[test]
    #[tracing_test::traced_test]
    fn boolean_capability_widening_warns_for_every_field() {
        use wafer_block::{capabilities::ConfigCapabilityOverrides, BlockCapabilities};

        let overrides = ConfigCapabilityOverrides {
            raw_sql: Some(true),
            ddl: Some(true),
            schema: Some(true),
            crypto: Some(true),
            ..Default::default()
        };
        let effective = BlockCapabilities::none();

        super::log_widening_attempts("test/widen-bools", &overrides, &effective);

        logs_assert(|lines: &[&str]| {
            for field in ["raw_sql", "ddl", "schema", "crypto"] {
                if !lines.iter().any(|l| l.contains(field)) {
                    return Err(format!("expected a widening warning for `{field}`"));
                }
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tool_name_tests {
    //! Cross-block agent-tool-name uniqueness. `BlockInfo::validate` sees one
    //! block at a time, so a name two blocks both claim only becomes visible
    //! once every block is registered — which is what `seal()` is for.

    use std::sync::Arc;

    use wafer_block::{
        error::RuntimeError,
        streams::{input::InputStream, output::OutputStream},
        AuthLevel, Block, BlockEndpoint, BlockInfo, Context, LifecycleEvent, Message, WaferError,
    };

    use super::super::Wafer;

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

    fn wafer_with(infos: Vec<BlockInfo>) -> Wafer {
        let mut wafer = Wafer::empty();
        for info in infos {
            let name = info.name.clone();
            wafer
                .register_block(name, Arc::new(FakeInfoBlock { info }))
                .expect("register");
        }
        wafer
    }

    fn shop(endpoints: Vec<BlockEndpoint>) -> BlockInfo {
        BlockInfo::new("test/shop", "1.0.0", "http-handler@v1", "Shop").endpoints(endpoints)
    }

    #[test]
    fn accepts_distinct_tool_names_across_blocks() {
        let wafer = wafer_with(vec![
            shop(vec![BlockEndpoint::get("/b/shop/things")
                .auth(AuthLevel::Public)
                .agent_tool("list_things", "List things.")]),
            BlockInfo::new("test/admin", "1.0.0", "http-handler@v1", "Admin").endpoints(vec![
                BlockEndpoint::get("/b/admin/things")
                    .auth(AuthLevel::Admin)
                    .agent_tool("list_all_things", "List every thing."),
            ]),
        ]);
        assert!(wafer.fail_on_duplicate_tool_names().is_ok());
    }

    #[test]
    fn refuses_a_name_claimed_by_a_public_and_an_admin_endpoint() {
        // The collision the per-manifest census resolves *differently per
        // caller*: the public endpoint would be published to anonymous and
        // authenticated callers as the sole `get_thing`, and only an admin's
        // manifest would report the ambiguity. Refused at boot instead.
        let wafer = wafer_with(vec![
            shop(vec![BlockEndpoint::get("/b/shop/thing")
                .auth(AuthLevel::Public)
                .agent_tool("get_thing", "Public thing.")]),
            BlockInfo::new("test/admin", "1.0.0", "http-handler@v1", "Admin").endpoints(vec![
                BlockEndpoint::get("/b/admin/thing")
                    .auth(AuthLevel::Admin)
                    .agent_tool("get_thing", "Admin thing."),
            ]),
        ]);

        let err = wafer
            .fail_on_duplicate_tool_names()
            .expect_err("a cross-block collision must refuse boot");
        let RuntimeError::DuplicateToolNames(duplicates) = &err else {
            panic!("wrong variant: {err:?}");
        };
        assert_eq!(duplicates.len(), 1);
        assert_eq!(duplicates[0].name, "get_thing");
        assert_eq!(duplicates[0].claimants.len(), 2);

        let msg = err.to_string();
        for expected in [
            "get_thing",
            "block `test/admin` GET /b/admin/thing",
            "block `test/shop` GET /b/shop/thing",
        ] {
            assert!(msg.contains(expected), "message: {msg}");
        }
    }

    #[test]
    fn refuses_a_name_claimed_twice_inside_one_block() {
        let wafer = wafer_with(vec![shop(vec![
            BlockEndpoint::get("/b/shop/a")
                .auth(AuthLevel::Public)
                .agent_tool("get_thing", "First."),
            BlockEndpoint::get("/b/shop/b")
                .auth(AuthLevel::Public)
                .agent_tool("get_thing", "Second."),
        ])]);
        let err = wafer
            .fail_on_duplicate_tool_names()
            .expect_err("a same-block collision is a collision too");
        assert!(err.to_string().contains("get_thing"), "message: {err}");
    }

    #[test]
    fn aggregates_every_collision_rather_than_stopping_at_the_first() {
        // The same posture as `BlocksNotFound`: an operator sees the whole
        // punch list, not one name per boot attempt.
        let wafer = wafer_with(vec![shop(vec![
            BlockEndpoint::get("/b/shop/a")
                .auth(AuthLevel::Public)
                .agent_tool("get_thing", "First."),
            BlockEndpoint::get("/b/shop/b")
                .auth(AuthLevel::Public)
                .agent_tool("get_thing", "Second."),
            BlockEndpoint::get("/b/shop/c")
                .auth(AuthLevel::Public)
                .agent_tool("set_thing", "Third."),
            BlockEndpoint::post("/b/shop/d")
                .auth(AuthLevel::Public)
                .agent_tool("set_thing", "Fourth."),
        ])]);
        let err = wafer
            .fail_on_duplicate_tool_names()
            .expect_err("two collisions");
        let RuntimeError::DuplicateToolNames(duplicates) = &err else {
            panic!("wrong variant: {err:?}");
        };
        assert_eq!(
            duplicates
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            vec!["get_thing", "set_thing"],
            "sorted by name so the message is stable across boots"
        );
    }

    #[test]
    fn endpoints_that_did_not_opt_in_never_collide() {
        // Two endpoints can share everything else; only a declared
        // `agent_tool` name is a claim on the agent-facing namespace.
        let wafer = wafer_with(vec![shop(vec![
            BlockEndpoint::get("/b/shop/a").auth(AuthLevel::Public),
            BlockEndpoint::get("/b/shop/b").auth(AuthLevel::Public),
        ])]);
        assert!(wafer.fail_on_duplicate_tool_names().is_ok());
    }

    #[tokio::test]
    async fn seal_refuses_to_boot_on_a_duplicate_tool_name() {
        // The gate is wired into the boot funnel, not merely available.
        let mut wafer = wafer_with(vec![shop(vec![
            BlockEndpoint::get("/b/shop/a")
                .auth(AuthLevel::Public)
                .agent_tool("get_thing", "First."),
            BlockEndpoint::get("/b/shop/b")
                .auth(AuthLevel::Admin)
                .agent_tool("get_thing", "Second."),
        ])]);
        let err = wafer.seal().await.expect_err("seal must refuse");
        assert!(
            matches!(err, RuntimeError::DuplicateToolNames(_)),
            "wrong variant: {err:?}"
        );
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
