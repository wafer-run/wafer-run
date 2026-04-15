//! Context trait re-exported from wafer-block. RuntimeContext stays here.

use std::collections::HashMap;
use std::sync::Arc;

use wafer_block::types::ResourceGrant;

use wafer_block::streams::input::InputStream;
use wafer_block::streams::output::OutputStream;

use crate::block::Block;
use crate::platform::Instant;
use crate::types::*;

// Re-export the trait from wafer-block.
pub use wafer_block::context::Context;

/// RuntimeContext implements Context for blocks.
///
/// Compiles on both native and wasm32 targets. Uses `web-time::Instant`
/// for deadline tracking (zero-cost on native, Performance.now() on wasm32).
pub struct RuntimeContext {
    pub flow_id: String,
    pub node_id: String,
    pub config: HashMap<String, String>,
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

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Context for RuntimeContext {
    async fn call_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream {
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
                    format!(
                        "block '{}' not in requires list — call_block denied",
                        block_name
                    ),
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
                        format!("block capability denies call to '{}'", block_name),
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
                                "block capability denies access to {} '{}'",
                                wrap_rt_str, wrap_resource
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
                    format!("block '{}' not found", block_name),
                );
            }
        };

        // Derive the called block's requires for its own sub-context
        let called_requires = {
            let info = block.info();
            if info.requires.is_empty() {
                None // unrestricted
            } else {
                Some(info.requires)
            }
        };

        // Build a sub-context for the called block
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
            aliases: self.aliases.clone(),
            caller_requires: called_requires,
            caller_id: Some(self.node_id.clone()),
            wrap_grants: self.wrap_grants.clone(),
            wrap_admin_block: self.wrap_admin_block.clone(),
        };

        // Call the block — _depth_guard drops after this, decrementing counter
        block.handle(&sub_ctx, msg, input).await
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
}
