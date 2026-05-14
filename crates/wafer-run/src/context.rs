//! Context trait re-exported from wafer-block. RuntimeContext stays here.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

// Re-export the trait from wafer-block.
pub use wafer_block::context::Context;
use wafer_block::{
    core_types::Attachment,
    streams::{input::InputStream, output::OutputStream},
    types::ResourceGrant,
};

use crate::{block::Block, platform::Instant, types::*};

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
    pub flow_id: String,
    pub node_id: String,
    pub config: Arc<HashMap<String, String>>,
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
    pub deadline: Option<Instant>,
    /// All registered blocks.
    pub all_blocks: Arc<HashMap<String, Arc<dyn Block>>>,
    /// Current call depth to prevent infinite recursion.
    pub call_depth: Arc<std::sync::atomic::AtomicU32>,
    /// Maximum call depth (default: 16).
    pub max_call_depth: u32,
    /// Snapshot of registered block info (populated at start time).
    pub registered_blocks_snapshot: Arc<Vec<crate::block::BlockInfo>>,
    /// Snapshot of flow info (populated at start time).
    pub flow_infos_snapshot: Arc<Vec<wafer_flow::FlowInfo>>,
    /// Snapshot of flow definitions (populated at start time).
    pub flow_defs_snapshot: Arc<Vec<wafer_flow::WaferFlow>>,
    /// Snapshot of expanded block configs (populated at start time).
    pub block_configs_snapshot: Arc<HashMap<String, serde_json::Value>>,
    /// Snapshot of interface specifications.
    pub interface_specs_snapshot: Arc<Vec<wafer_block::InterfaceSpec>>,
    /// Warn-once tracking for unknown interfaces. Shared Arc with the Wafer.
    pub warned_unknown_interfaces: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
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
                ErrorCode::RESOURCE_EXHAUSTED,
                format!(
                    "call_block depth exceeded maximum of {} (calling '{}')",
                    self.max_call_depth, block_name
                ),
            );
        }

        // Cancellation check
        if self.is_cancelled() {
            return err_output(ErrorCode::CANCELLED, "execution cancelled");
        }

        // Enforce requires: if the caller declared a requires list, check it
        let resolved_name = self
            .aliases
            .get(block_name)
            .map(|s| s.as_str())
            .unwrap_or(block_name);
        if let Some(ref requires) = self.caller_requires {
            if !requires
                .iter()
                .any(|r| r == block_name || r == resolved_name)
            {
                return err_output(
                    ErrorCode::PERMISSION_DENIED,
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
                // Check call_block capability
                if !caps.allows_call_block(block_name) {
                    return err_output(
                        ErrorCode::PERMISSION_DENIED,
                        format!("block capability denies call to '{block_name}'"),
                    );
                }

                // Check resource-specific capabilities based on resource_type meta
                let wrap_rt_str = msg.get_meta(wafer_block::meta::META_WRAP_RESOURCE_TYPE);
                if !wrap_rt_str.is_empty() {
                    let wrap_resource = msg.get_meta(wafer_block::meta::META_WRAP_RESOURCE);
                    let allowed = match wrap_rt_str {
                        "db" => {
                            if wrap_resource == "__raw_sql__" {
                                caps.raw_sql
                            } else if wrap_resource == "__ddl__" {
                                caps.ddl
                            } else {
                                caps.allows_collection(wrap_resource)
                            }
                        }
                        "storage" => caps.allows_storage_folder(wrap_resource),
                        "config" => caps.config && caps.allows_config_key(wrap_resource),
                        "crypto" => caps.crypto,
                        "network" => caps.allows_network_url(wrap_resource),
                        _ => true,
                    };
                    if !allowed {
                        return err_output(
                            ErrorCode::PERMISSION_DENIED,
                            format!(
                                "block capability denies access to {wrap_rt_str} '{wrap_resource}'"
                            ),
                        );
                    }
                }
            }
        }

        // Look up the block (try aliases then direct name)
        let resolved_block_name = self
            .aliases
            .get(block_name)
            .map(|s| s.as_str())
            .unwrap_or(block_name);
        let block = match self
            .all_blocks
            .get(resolved_block_name)
            .or_else(|| self.all_blocks.get(block_name))
        {
            Some(b) => b.clone(),
            None => {
                return err_output(
                    ErrorCode::NOT_FOUND,
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
                &self.interface_specs_snapshot,
            ) {
                crate::runtime::validation::ActionCheck::Valid => {}
                crate::runtime::validation::ActionCheck::Invalid { message } => {
                    return err_output(ErrorCode::INVALID_ARGUMENT, message);
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

        let sub_ctx = RuntimeContext {
            flow_id: self.flow_id.clone(),
            node_id: block_name.to_string(),
            config: self.config.clone(),
            cancelled: self.cancelled.clone(),
            deadline: self.deadline,
            all_blocks: self.all_blocks.clone(),
            call_depth: self.call_depth.clone(),
            max_call_depth: self.max_call_depth,
            registered_blocks_snapshot: self.registered_blocks_snapshot.clone(),
            flow_infos_snapshot: self.flow_infos_snapshot.clone(),
            flow_defs_snapshot: self.flow_defs_snapshot.clone(),
            block_configs_snapshot: self.block_configs_snapshot.clone(),
            interface_specs_snapshot: self.interface_specs_snapshot.clone(),
            warned_unknown_interfaces: self.warned_unknown_interfaces.clone(),
            aliases: self.aliases.clone(),
            caller_requires: called_requires,
            caller_id: Some(self.node_id.clone()),
            wrap_grants: self.wrap_grants.clone(),
            wrap_admin_block: self.wrap_admin_block.clone(),
            current_attachments: sub_attachments,
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

        // _depth_guard drops after this, decrementing counter.
        block.handle(&sub_ctx, msg, input).await
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
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
        self.config.get(key).map(|s| s.as_str())
    }

    fn registered_blocks(&self) -> Vec<crate::block::BlockInfo> {
        (*self.registered_blocks_snapshot).clone()
    }

    fn flow_infos(&self) -> Vec<wafer_flow::FlowInfo> {
        (*self.flow_infos_snapshot).clone()
    }

    fn flow_defs(&self) -> Vec<wafer_flow::WaferFlow> {
        (*self.flow_defs_snapshot).clone()
    }

    fn block_configs(&self) -> std::collections::HashMap<String, serde_json::Value> {
        (*self.block_configs_snapshot).clone()
    }

    fn interface_specs(&self) -> Vec<wafer_block::InterfaceSpec> {
        (*self.interface_specs_snapshot).clone()
    }

    fn caller_id(&self) -> Option<&str> {
        self.caller_id.as_deref()
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}
