//! Context trait re-exported from wafer-block. RuntimeContext stays here.

use std::collections::HashMap;
use std::sync::Arc;

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
    /// Alias mappings (e.g. `"@db"` → `"solobase/sqlite"`).
    pub aliases: Arc<HashMap<String, String>>,
    /// Block names the caller is allowed to call via `call_block()`.
    /// `None` means unrestricted. `Some(list)` enforces the allowlist.
    pub caller_requires: Option<Vec<String>>,
}

// --- Result helpers (used by RuntimeContext impl) ---

fn err_result(code: ErrorCode, message: impl Into<String>) -> Result_ {
    Result_ {
        action: Action::Error,
        response: None,
        error: Some(WaferError::new(code, message)),
        message: None,
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Context for RuntimeContext {
    async fn call_block(&self, block_name: &str, msg: &mut Message) -> Result_ {
        // Recursion depth check
        let depth = self
            .call_depth
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if depth >= self.max_call_depth {
            self.call_depth
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            return err_result(
                ErrorCode::RESOURCE_EXHAUSTED,
                format!(
                    "call_block depth exceeded maximum of {} (calling '{}')",
                    self.max_call_depth, block_name
                ),
            );
        }

        // Cancellation check
        if self.is_cancelled() {
            self.call_depth
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            return err_result(ErrorCode::CANCELLED, "execution cancelled");
        }

        // Enforce requires: if the caller declared a requires list, check it
        let resolved_name = self.aliases.get(block_name)
            .map(|s| s.as_str())
            .unwrap_or(block_name);
        if let Some(ref requires) = self.caller_requires {
            if !requires.iter().any(|r| r == block_name || r == resolved_name) {
                self.call_depth
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                return err_result(
                    ErrorCode::PERMISSION_DENIED,
                    format!(
                        "block '{}' not in requires list — call_block denied",
                        block_name
                    ),
                );
            }
        }

        // Look up the block
        let block = match self.all_blocks.get(block_name) {
            Some(b) => b.clone(),
            None => {
                self.call_depth
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                return err_result(
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
            aliases: self.aliases.clone(),
            caller_requires: called_requires,
        };

        // Call the block
        let result = block.handle(&sub_ctx, msg).await;

        self.call_depth
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        result
    }

    fn is_cancelled(&self) -> bool {
        if self
            .cancelled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
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
}
