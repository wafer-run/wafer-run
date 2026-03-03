use std::collections::HashMap;
use std::sync::Arc;

use crate::block::Block;
use crate::common::ErrorCode;
use crate::types::*;

/// Context provides runtime capabilities to blocks.
pub trait Context: Send + Sync {
    /// Call another block by name.
    fn call_block(&self, block_name: &str, msg: &mut Message) -> Result_;

    /// Check if the context has been cancelled.
    fn is_cancelled(&self) -> bool;

    /// Get a config value from the block's node config.
    fn config_get(&self, key: &str) -> Option<&str>;
}

/// RuntimeContext implements Context for blocks.
pub struct RuntimeContext {
    pub chain_id: String,
    pub node_id: String,
    pub config: HashMap<String, String>,
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
    pub deadline: Option<std::time::Instant>,
    /// All registered blocks (infrastructure + application).
    pub all_blocks: Arc<HashMap<String, Arc<dyn Block>>>,
    /// Current call depth to prevent infinite recursion.
    pub call_depth: Arc<std::sync::atomic::AtomicU32>,
    /// Maximum call depth (default: 16).
    pub max_call_depth: u32,
}

// --- Result helpers ---

fn err_result(code: impl Into<String>, message: impl Into<String>) -> Result_ {
    Result_ {
        action: Action::Error,
        response: None,
        error: Some(WaferError::new(code, message)),
        message: None,
    }
}

impl Context for RuntimeContext {
    fn call_block(&self, block_name: &str, msg: &mut Message) -> Result_ {
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

        // Build a sub-context for the called block
        let sub_ctx = RuntimeContext {
            chain_id: self.chain_id.clone(),
            node_id: block_name.to_string(),
            config: self.config.clone(),
            cancelled: self.cancelled.clone(),
            deadline: self.deadline,
            all_blocks: self.all_blocks.clone(),
            call_depth: self.call_depth.clone(),
            max_call_depth: self.max_call_depth,
        };

        // Call the block
        let result = block.handle(&sub_ctx, msg);

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
}
