//! Context trait re-exported from wafer-block. RuntimeContext stays here.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

// Re-export the trait from wafer-block.
pub use wafer_block::context::Context;
use wafer_block::{
    core_types::*,
    streams::{input::InputStream, output::OutputStream},
    types::*,
    Block,
};
use wafer_block_macro::wafer_async_trait;

use crate::platform::Instant;

/// RuntimeContext implements Context for blocks.
///
/// Compiles on both native and wasm32 targets. Uses `web-time::Instant`
/// for deadline tracking (zero-cost on native, Performance.now() on wasm32).
///
/// `Clone` is cheap — every field is either `Arc<...>`, `Option<...>`, a
/// small `Copy` value, or a `String`. Cloning produces a new owning handle
/// that points at the same shared snapshots; used by [`Context::clone_arc`].
#[derive(Clone)]
pub struct RuntimeContext {
    /// Identifier of the flow currently executing (matches `WaferFlow::id`).
    pub flow_id: String,
    /// Identifier of the node within the flow that triggered this context.
    pub node_id: String,
    /// Per-call config overrides — flow-step `step.config`, per-frame
    /// caller-supplied keys, etc. Wins over [`Self::config_snapshot`] in
    /// [`Context::config_get`]. Made `pub(crate)` so [`Context::config_get`]
    /// is the only access path; if callers need raw map access in the future,
    /// add a method that returns a layered view rather than re-exposing the
    /// field.
    pub(crate) config: Arc<HashMap<String, String>>,
    /// Embedder-supplied env-style configuration snapshot, shared `Arc` with
    /// the [`Wafer`] that produced this context (see
    /// [`Wafer::set_config_snapshot`]). Layered under [`Self::config`] —
    /// override wins. Empty by default; populated by consumers such as
    /// `solobase-cloudflare` and `solobase-native` once they have loaded
    /// env vars / D1 variables at boot.
    pub(crate) config_snapshot: Arc<HashMap<String, String>>,
    /// Shared cancellation flag — set by the runtime when the flow is aborted.
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
    /// Optional wall-clock deadline after which the runtime will signal cancellation.
    pub deadline: Option<Instant>,
    /// All registered blocks.
    pub all_blocks: Arc<HashMap<String, Arc<dyn Block>>>,
    /// Current call depth to prevent infinite recursion.
    pub call_depth: Arc<std::sync::atomic::AtomicU32>,
    /// Maximum call depth (default: 16).
    pub max_call_depth: u32,
    /// Immutable bundle of post-startup metadata: registered blocks,
    /// flow infos/defs, expanded block configs, interface specs.
    pub snapshot: Arc<crate::snapshot::StartupSnapshot>,
    /// Warn-once tracking for unknown interfaces. Shared Arc with the Wafer.
    pub warned_unknown_interfaces: Arc<parking_lot::Mutex<std::collections::HashSet<String>>>,
    /// Alias mappings (e.g. `"@db"` → `"solobase/sqlite"`).
    pub aliases: Arc<HashMap<String, String>>,
    /// Block names the caller is allowed to call via `call_block()`.
    /// `None` means unrestricted. `Some(list)` enforces the allowlist.
    pub caller_requires: Option<Vec<String>>,
    /// The block name of the caller that invoked this block via `call_block()`.
    /// `None` for top-level calls (e.g. from the router).
    pub caller_id: Option<String>,
    /// WRAP: all validated resource grants collected at startup.
    pub wrap_grants: Arc<Vec<ResourceGrant>>,
    /// WRAP: the block ID that has admin privileges (exact match).
    pub wrap_admin_block: Arc<String>,
    /// Per-call-frame inbound attachments. Populated when this context was
    /// produced by `call_block_with_attachments` on the caller side; consulted
    /// by `lookup_attachment`. Empty for top-level calls and for `call_block`
    /// (without attachments).
    pub current_attachments: Arc<BTreeMap<String, Attachment>>,
    /// Per-top-level-dispatch init breadcrumbs for cycle detection.
    /// Fresh stack at each `Wafer::run`; nested dispatches inherit via clone
    /// of the inner `Arc<Mutex<Vec<String>>>`, so all frames share state.
    /// Used by `Wafer::init_block` to surface `InitError::Cycle`.
    pub(crate) init_breadcrumbs: crate::runtime::init_stack::InitStack,
    /// Snapshot of the runtime's per-block init slots. Shared via `Arc` with
    /// [`Wafer::slots`]; consulted by [`RuntimeContext::dispatch_call`] to
    /// drive lazy init on `call_block` callees. Empty (default) when the
    /// context is built outside the runtime (e.g. in standalone tests).
    pub(crate) slots: Arc<HashMap<String, Arc<crate::runtime::slot::BlockSlot>>>,
    /// Source of per-block env-var config. Same `Arc` held by [`Wafer`];
    /// consulted by `dispatch_call` to drive lazy init of `call_block`
    /// callees.
    pub(crate) config_source: Arc<dyn crate::runtime::config_source::ConfigSource>,
    /// Observability hook bus, `Arc`-shared with the [`Wafer`] that produced
    /// this context. Consulted by `dispatch_call` so nested block-to-block
    /// calls fire the same `block_start`/`block_end` hooks as the top-level
    /// dispatch paths.
    pub(crate) hooks: Arc<crate::observability::ObservabilityBus>,
}

// --- Output helpers (used by RuntimeContext impl) ---

fn err_output(code: ErrorCode, message: impl Into<String>) -> OutputStream {
    OutputStream::error(WaferError::new(code, message))
}

/// RAII guard that decrements `call_depth` on drop, even if the block panics.
struct CallDepthGuard(Arc<std::sync::atomic::AtomicU32>);

impl Drop for CallDepthGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl RuntimeContext {
    /// Resolve `name` through the alias map, single-hop. Mirrors
    /// [`crate::Wafer::canonicalize`]. Single-hop is sufficient because
    /// [`crate::Wafer::add_alias`] rejects chained registrations.
    pub(crate) fn canonicalize<'a>(&'a self, name: &'a str) -> &'a str {
        self.aliases.get(name).map_or(name, |s| s.as_str())
    }

    /// Shared dispatch path used by both `call_block` and
    /// `call_block_with_attachments`. Performs the full validation pipeline
    /// (depth, cancellation, requires, WRAP, capabilities, interface action),
    /// then builds a sub-context with `current_attachments` populated and
    /// dispatches.
    ///
    /// For wasmi callees, `attachments.is_some()` triggers the
    /// `WasmiBlock::handle_with_attachments` path so the wasmi store's
    /// `current_attachments` slot is seeded before `__wafer_handle` runs;
    /// `None` (the `call_block` case) takes the regular `Block::handle` path.
    async fn dispatch_call(
        &self,
        block_name: &str,
        msg: Message,
        input: InputStream,
        attachments: Option<BTreeMap<String, Attachment>>,
    ) -> OutputStream {
        // Recursion depth check — the RAII guard ensures the counter is
        // decremented even if the block panics.
        let depth = self
            .call_depth
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _depth_guard = CallDepthGuard(self.call_depth.clone());

        if depth >= self.max_call_depth {
            return err_output(
                ErrorCode::ResourceExhausted,
                format!(
                    "call_block depth exceeded maximum of {} (calling '{}')",
                    self.max_call_depth, block_name
                ),
            );
        }

        // Cancellation check
        if self.is_cancelled() {
            return err_output(ErrorCode::Cancelled, "execution cancelled");
        }

        // Resolve the alias once up front. This canonical name is the caller
        // identity used for every downstream decision: the callee's sub-context
        // `node_id` (WRAP attribution), the capability `allows_call_block`
        // membership check, and the block lookup. Computing it once avoids the
        // earlier bug where some sites compared/attributed against the raw
        // alias and others against the resolved name.
        let resolved_block_name = self.canonicalize(block_name);

        // Enforce requires: if the caller declared a requires list, check it.
        // Match against BOTH the raw alias the caller wrote and the resolved
        // canonical name, so a `requires` entry written either way is honored.
        if let Some(ref requires) = self.caller_requires {
            if !requires
                .iter()
                .any(|r| r == block_name || r == resolved_block_name)
            {
                return err_output(
                    ErrorCode::PermissionDenied,
                    format!("block '{block_name}' not in requires list — call_block denied"),
                );
            }
        }

        // WRAP: check resource access if wrap.resource meta is set.
        let wrap_resource = msg.get_meta(wafer_block::meta::META_WRAP_RESOURCE);
        if !wrap_resource.is_empty() {
            let is_write = msg.get_meta(wafer_block::meta::META_WRAP_ACCESS) == "write";
            let wrap_rt_str = msg.get_meta(wafer_block::meta::META_WRAP_RESOURCE_TYPE);
            let wrap_rt = if wrap_rt_str.is_empty() {
                None
            } else {
                wafer_block::types::ResourceType::parse(wrap_rt_str)
            };
            let wrap_caller = if self.node_id.is_empty() {
                self.caller_id.as_deref()
            } else {
                Some(self.node_id.as_str())
            };
            if let Err(e) = wafer_block::wrap::check_access(
                wrap_caller,
                wrap_resource,
                is_write,
                wrap_rt.as_ref(),
                &self.wrap_grants,
                &self.wrap_admin_block,
            ) {
                return err_output(e.code, e.message);
            }
        }

        // Capability check: if the calling block is a WASM block with restricted
        // capabilities, verify it has permission for this service call.
        if let Some(caller_block) = self.all_blocks.get(&self.node_id) {
            if let Some(caps) = caller_block.block_capabilities() {
                // Check call_block capability. `allows_call_block` does exact
                // membership against the canonical names a block declares in
                // its capabilities, so it must see the resolved name — an alias
                // would never match and produce a false denial.
                if !caps.allows_call_block(resolved_block_name) {
                    return err_output(
                        ErrorCode::PermissionDenied,
                        format!("block capability denies call to '{block_name}'"),
                    );
                }

                // Check resource-specific capabilities based on resource_type
                // meta. Parse the type string into `ResourceType` once and match
                // on the enum variants — same approach as the WRAP check above,
                // instead of re-matching hardcoded type strings here.
                let wrap_rt_str = msg.get_meta(wafer_block::meta::META_WRAP_RESOURCE_TYPE);
                if let Some(wrap_rt) = wafer_block::types::ResourceType::parse(wrap_rt_str) {
                    use wafer_block::types::ResourceType;
                    let wrap_resource = msg.get_meta(wafer_block::meta::META_WRAP_RESOURCE);
                    let allowed = match wrap_rt {
                        ResourceType::Db => {
                            if wrap_resource == wafer_block::wrap::RAW_SQL_RESOURCE {
                                caps.raw_sql
                            } else if wrap_resource == wafer_block::wrap::DDL_RESOURCE {
                                caps.ddl
                            } else {
                                caps.allows_collection(wrap_resource)
                            }
                        }
                        ResourceType::Storage => caps.allows_storage_folder(wrap_resource),
                        ResourceType::Config => {
                            caps.config && caps.allows_config_key(wrap_resource)
                        }
                        ResourceType::Crypto => caps.crypto,
                        ResourceType::Network => caps.allows_network_url(wrap_resource),
                    };
                    if !allowed {
                        return err_output(
                            ErrorCode::PermissionDenied,
                            format!(
                                "block capability denies access to {wrap_rt} '{wrap_resource}'"
                            ),
                        );
                    }
                }
            }
        }

        // Look up the block (resolved canonical name first, then the raw name
        // as a defensive fallback — the same canonicalize-then-fallback the
        // flow runner and `Wafer::lookup_block` use). `resolved_block_name` was
        // computed once near the top of this fn.
        let block = match self
            .all_blocks
            .get(resolved_block_name)
            .or_else(|| self.all_blocks.get(block_name))
        {
            Some(b) => b.clone(),
            None => {
                return err_output(
                    ErrorCode::NotFound,
                    format!("block '{block_name}' not found"),
                );
            }
        };

        // Interface action validation: verify the message action is part of the
        // target block's declared interface. Skipped for action-agnostic
        // interfaces (empty action map) and for interfaces the runtime does
        // not recognize (warn-once, then proceed).
        //
        // Two callers populate the action field, in two different places:
        //   - HTTP listener: maps `POST` → `req.action` meta = `"create"` etc.
        //     `kind` carries the composite `"METHOD:/path"` for routing.
        //   - SDK clients (`wafer_sdk::clients::*`): set `kind` to the service
        //     op (e.g. `"network.do"`); the meta entry is not populated.
        // Prefer the meta value (semantic action) when present; fall back to
        // `kind` (the SDK op name). This keeps a single validation lookup that
        // works for both call-paths without forcing the SDK to duplicate kind
        // into meta on every call.
        let info = block.info();
        {
            let action_meta = msg.action();
            let action = if !action_meta.is_empty() {
                action_meta
            } else {
                msg.kind.as_str()
            };
            match crate::runtime::validation::check_action_interface(
                &info.name,
                &info.interface,
                action,
                &self.snapshot.interface_specs,
            ) {
                crate::runtime::validation::ActionCheck::Valid => {}
                crate::runtime::validation::ActionCheck::Invalid { message } => {
                    return err_output(ErrorCode::InvalidArgument, message);
                }
                crate::runtime::validation::ActionCheck::UnknownInterface => {
                    crate::runtime::validation::warn_once_unknown_interface(
                        &self.warned_unknown_interfaces,
                        &info.name,
                        &info.interface,
                    );
                }
            }
        }

        // Derive the called block's requires for its own sub-context
        let called_requires = if info.requires.is_empty() {
            None // unrestricted
        } else {
            Some(info.requires)
        };

        // Wrap attachments in an Arc once, consuming the BTreeMap — no deep clone.
        let att_arc: Option<Arc<BTreeMap<String, Attachment>>> = attachments.map(Arc::new);

        // For wasmi callees, sub_ctx.current_attachments is never consulted —
        // the wasmi store slot is seeded separately by
        // `handle_with_attachments`. Give sub_ctx an empty Arc so that
        // att_arc remains the sole holder and Arc::try_unwrap succeeds later
        // (avoiding a deep clone of Attachment::bytes on the wasmi hot path).
        // For native callees, share a cheap Arc::clone of the populated map.
        #[cfg(feature = "wasmi")]
        let is_wasmi_callee = block
            .as_any()
            .and_then(|a| a.downcast_ref::<crate::wasm::WasmiBlock>())
            .is_some();
        #[cfg(not(feature = "wasmi"))]
        let is_wasmi_callee = false;

        let sub_attachments: Arc<BTreeMap<String, Attachment>> = if is_wasmi_callee {
            // wasmi path: callee reads from WasmiHostState slot, not sub_ctx.
            Arc::new(BTreeMap::new())
        } else {
            // Native path: share the Arc (or empty if no attachments).
            att_arc.clone().unwrap_or_else(|| Arc::new(BTreeMap::new()))
        };

        // The sub-context is a clone of this frame with only the four
        // caller/callee-identity fields overridden — every shared snapshot
        // (config, blocks, slots, WRAP state, breadcrumbs, ...) is inherited
        // via `RuntimeContext::clone` (cheap: Arc/Copy/String fields only):
        //   - node_id: the callee's identity for downstream WRAP attribution
        //     (resource owner is `{org}/{block}`). It must be the resolved
        //     canonical name, not the raw alias the caller wrote — otherwise
        //     an aliased callee performing its own resource access would be
        //     attributed to the alias and falsely denied.
        //   - caller_requires: the callee's own `requires` allowlist.
        //   - caller_id: this frame's node becomes the callee's caller.
        //   - current_attachments: the per-call inbound attachment map.
        let sub_ctx = RuntimeContext {
            node_id: resolved_block_name.to_string(),
            caller_requires: called_requires,
            caller_id: Some(self.node_id.clone()),
            current_attachments: sub_attachments,
            ..self.clone()
        };

        // Lazy init + observability bracket via the shared dispatch scaffold
        // (`run_resolved`, shared with `Wafer::run_block` and the flow
        // executor). Init inherits `self.init_breadcrumbs` so a cycle
        // (block A.init -> A.handle -> call_block(B) where B.init -> ... -> A)
        // is detected; the init pipeline pushes `resolved_block_name` onto
        // the stack inside `run_init_pipeline`.
        //
        // Every registered block has a paired slot (`register_block_inner` /
        // `register_remote_block`). A missing entry here means
        // `resolved_block_name` was found in `self.blocks` but not
        // `self.slots` — a runtime invariant violation, so panic loudly
        // rather than silently constructing a fresh slot (which would let
        // concurrent callers each run `lifecycle(Init)` independently).
        let init = crate::runtime::runner::DispatchInit {
            block: block.clone(),
            slot: self
                .slots
                .get(resolved_block_name)
                .cloned()
                .expect("slot must exist for any registered block"),
            config_source: self.config_source.clone(),
            init_ctx: sub_ctx.clone(),
            stack: &self.init_breadcrumbs,
        };

        // Dispatch. For wasmi callees with attachments, route through
        // `WasmiBlock::handle_with_attachments` so the per-call slot in
        // `WasmiHostState` is seeded before `__wafer_handle` runs. Without
        // attachments the regular `Block::handle` path is used (the wasmi
        // host-state slot stays `None`).
        //
        // Because sub_ctx holds an *empty* Arc (not a clone of att_arc), the
        // Arc::try_unwrap below succeeds without a deep clone — att_arc is the
        // sole holder of the BTreeMap at this point.
        //
        // _depth_guard drops after this, decrementing counter.
        crate::runtime::runner::run_resolved(
            &self.hooks,
            crate::runtime::runner::DispatchObs {
                flow_id: &self.flow_id,
                node_path: resolved_block_name,
                block_name,
            },
            resolved_block_name,
            init,
            msg,
            input,
            move |msg, input| async move {
                #[cfg(feature = "wasmi")]
                if let Some(arc) = att_arc {
                    if let Some(any) = block.as_any() {
                        if let Some(wasmi_block) = any.downcast_ref::<crate::wasm::WasmiBlock>() {
                            let map = Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone());
                            return wasmi_block
                                .handle_with_attachments(&sub_ctx, msg, input, map)
                                .await;
                        }
                    }
                }
                block.handle(&sub_ctx, msg, input).await
            },
        )
        .await
        .unwrap_or_else(|init_failure| init_failure)
    }
}

#[wafer_async_trait]
impl Context for RuntimeContext {
    /// Dispatch a message to another registered block.
    ///
    /// # Checks
    ///
    /// Runs in order, returning an error event on failure:
    /// 1. Call-depth limit (default 16).
    /// 2. Cancellation / deadline.
    /// 3. Caller `requires` allowlist.
    /// 4. WRAP resource access (`META_WRAP_RESOURCE`).
    /// 5. Caller capability check (WASM capability model).
    /// 6. **Interface action**: `msg.action()` must be in the target block's
    ///    declared interface action map, unless the interface is
    ///    action-agnostic (empty map) or unknown to the runtime. Unknown
    ///    interfaces produce a one-time `WARN` log per block.
    ///
    /// See `crates/wafer-run/src/runtime/validation.rs`.
    async fn call_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream {
        self.dispatch_call(block_name, msg, input, None).await
    }

    async fn call_block_with_attachments(
        &self,
        block_name: &str,
        msg: Message,
        input: InputStream,
        attachments: BTreeMap<String, Attachment>,
    ) -> OutputStream {
        self.dispatch_call(block_name, msg, input, Some(attachments))
            .await
    }

    fn lookup_attachment(&self, id: &str) -> Option<Attachment> {
        self.current_attachments.get(id).cloned()
    }

    fn is_cancelled(&self) -> bool {
        if self.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            return true;
        }
        if let Some(deadline) = self.deadline {
            if Instant::now() >= deadline {
                self.cancelled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                return true;
            }
        }
        false
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        // Per-call overrides win; embedder snapshot is the fallback.
        self.config
            .get(key)
            .or_else(|| self.config_snapshot.get(key))
            .map(|s| s.as_str())
    }

    fn registered_blocks(&self) -> Vec<wafer_block::BlockInfo> {
        self.snapshot.blocks.clone()
    }

    fn flow_introspection(&self) -> Option<&dyn wafer_block::introspection::FlowIntrospection> {
        Some(self)
    }

    fn block_configs(&self) -> std::collections::HashMap<String, serde_json::Value> {
        self.snapshot.block_configs.clone()
    }

    fn interface_specs(&self) -> Vec<wafer_block::InterfaceSpec> {
        self.snapshot.interface_specs.clone()
    }

    fn caller_id(&self) -> Option<&str> {
        self.caller_id.as_deref()
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }

    async fn validate_all_block_configs(&self) -> wafer_block::ValidationReport {
        // `all_blocks` contains registered blocks AND alias keys pointing at
        // the same instances. Filter the alias keys out so the shared
        // validator sees the canonical view — otherwise aliased blocks would
        // be validated and reported twice (once per name). Collected up front
        // because a borrowed `filter` closure does not generalize across the
        // validator's async boundary.
        let canonical: Vec<(&String, &Arc<dyn Block>)> = self
            .all_blocks
            .iter()
            .filter(|(name, _)| !self.aliases.contains_key(*name))
            .collect();
        crate::runtime::config_source::validate_block_configs(canonical, &self.config_source).await
    }
}

/// JSON pass-through introspection over the live startup snapshot.
///
/// Mirrors what the deleted `Context::flow_infos`/`flow_defs` returned, but
/// serialized to `serde_json::Value` so `wafer-block` does not need a
/// `wafer-flow` dependency to expose the data.
impl wafer_block::introspection::FlowIntrospection for RuntimeContext {
    fn flow_infos_json(&self) -> Vec<serde_json::Value> {
        self.snapshot
            .flow_infos
            .iter()
            .filter_map(|info| match serde_json::to_value(info) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        flow_id = %info.id,
                        "flow info serialization failed; dropped from introspection"
                    );
                    None
                }
            })
            .collect()
    }

    fn flow_defs_json(&self) -> Vec<serde_json::Value> {
        self.snapshot
            .flow_defs
            .iter()
            .filter_map(|def| match serde_json::to_value(def) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        flow_id = %def.id,
                        "flow def serialization failed; dropped from introspection"
                    );
                    None
                }
            })
            .collect()
    }
}
