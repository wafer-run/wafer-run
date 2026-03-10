use crate::types::*;

#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use crate::block::Block;
#[cfg(not(target_arch = "wasm32"))]
use crate::common::ErrorCode;

/// Context provides runtime capabilities to blocks.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait Context: Send + Sync {
    /// Call another block by name.
    async fn call_block(&self, block_name: &str, msg: &mut Message) -> Result_;

    /// Check if the context has been cancelled.
    fn is_cancelled(&self) -> bool;

    /// Get a config value from the block's node config.
    fn config_get(&self, key: &str) -> Option<&str>;

    /// List all registered blocks.
    fn registered_blocks(&self) -> Vec<crate::block::BlockInfo> { Vec::new() }

    /// List flow summary info.
    fn flow_infos(&self) -> Vec<crate::config::FlowInfo> { Vec::new() }

    /// List full flow definitions.
    fn flow_defs(&self) -> Vec<crate::config::FlowDef> { Vec::new() }
}

/// RuntimeContext implements Context for blocks.
/// Only available on non-wasm32 targets (uses std::time::Instant).
#[cfg(not(target_arch = "wasm32"))]
pub struct RuntimeContext {
    pub flow_id: String,
    pub node_id: String,
    pub config: HashMap<String, String>,
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
    pub deadline: Option<std::time::Instant>,
    /// All registered blocks.
    pub all_blocks: Arc<HashMap<String, Arc<dyn Block>>>,
    /// Current call depth to prevent infinite recursion.
    pub call_depth: Arc<std::sync::atomic::AtomicU32>,
    /// Maximum call depth (default: 16).
    pub max_call_depth: u32,
    /// Snapshot of registered block info (populated at start time).
    pub registered_blocks_snapshot: Arc<Vec<crate::block::BlockInfo>>,
    /// Snapshot of flow info (populated at start time).
    pub flow_infos_snapshot: Arc<Vec<crate::config::FlowInfo>>,
    /// Snapshot of flow definitions (populated at start time).
    pub flow_defs_snapshot: Arc<Vec<crate::config::FlowDef>>,
    /// Alias mappings (e.g. `"@db"` → `"solobase/sqlite"`).
    pub aliases: Arc<HashMap<String, String>>,
    /// Block names the caller is allowed to call via `call_block()`.
    /// `None` means unrestricted. `Some(list)` enforces the allowlist.
    pub caller_requires: Option<Vec<String>>,
}

// --- Result helpers (used by RuntimeContext impl) ---

#[cfg(not(target_arch = "wasm32"))]
fn err_result(code: impl Into<String>, message: impl Into<String>) -> Result_ {
    Result_ {
        action: Action::Error,
        response: None,
        error: Some(WaferError::new(code, message)),
        message: None,
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
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
        // Resolve alias to target name so both alias and canonical name match
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
            if std::time::Instant::now() >= deadline {
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

    fn flow_infos(&self) -> Vec<crate::config::FlowInfo> {
        (*self.flow_infos_snapshot).clone()
    }

    fn flow_defs(&self) -> Vec<crate::config::FlowDef> {
        (*self.flow_defs_snapshot).clone()
    }
}
