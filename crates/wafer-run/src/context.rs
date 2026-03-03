use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use crate::block::Block;
use crate::common::ErrorCode;
use crate::services::Services;
use crate::types::*;

/// CapabilityInfo describes a runtime capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityInfo {
    pub kind: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
}

/// Context provides runtime capabilities to blocks.
pub trait Context: Send + Sync {
    /// Call another block by name.
    fn call_block(&self, block_name: &str, msg: &mut Message) -> Result_;

    /// Check if the context has been cancelled.
    fn is_cancelled(&self) -> bool;

    /// Get a config value from the block's node config.
    fn config_get(&self, key: &str) -> Option<&str>;

    // --- Legacy methods kept for backward compatibility during migration ---

    /// Send a message to a runtime capability (log, config, dispatch, etc.)
    fn send(&self, msg: &Message) -> Result_ {
        let _ = msg;
        Result_ {
            action: Action::Error,
            response: None,
            error: Some(WaferError::new(
                ErrorCode::UNIMPLEMENTED,
                "send() is deprecated, use call_block()",
            )),
            message: None,
        }
    }

    /// Capabilities returns available runtime capabilities.
    fn capabilities(&self) -> Vec<CapabilityInfo> {
        Vec::new()
    }

    /// Service returns a named service registered on the runtime, or None.
    fn service(&self, _name: &str) -> Option<&dyn Any> {
        None
    }

    /// Services returns the typed platform services.
    fn services(&self) -> Option<&Services> {
        None
    }
}

/// RuntimeContext implements Context for blocks.
pub struct RuntimeContext {
    pub chain_id: String,
    pub node_id: String,
    pub config: HashMap<String, String>,
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
    pub deadline: Option<std::time::Instant>,
    pub named_services: Arc<HashMap<String, Box<dyn Any + Send + Sync>>>,
    pub platform_services: Option<Arc<Services>>,
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
            named_services: self.named_services.clone(),
            platform_services: self.platform_services.clone(),
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

    // --- Legacy methods for backward compatibility ---

    fn send(&self, msg: &Message) -> Result_ {
        let kind = msg.kind.as_str();

        // Route svc.* messages to infrastructure blocks via call_block
        if let Some(svc_kind) = kind.strip_prefix("svc.") {
            // Map service kind to block name
            let block_name = if svc_kind.starts_with("database.") {
                "wafer/database"
            } else if svc_kind.starts_with("storage.") {
                "wafer/storage"
            } else if svc_kind.starts_with("crypto.") {
                "wafer/crypto"
            } else if svc_kind == "network.do" {
                "wafer/network"
            } else if svc_kind.starts_with("logger.") {
                "wafer/logger"
            } else if svc_kind.starts_with("config.") {
                "wafer/config"
            } else {
                return err_result(
                    ErrorCode::UNAVAILABLE,
                    format!("unknown service capability: svc.{svc_kind}"),
                );
            };

            // Forward to the infrastructure block with the service-specific kind
            let mut svc_msg = Message {
                kind: svc_kind.to_string(),
                data: msg.data.clone(),
                meta: msg.meta.clone(),
            };
            return self.call_block(block_name, &mut svc_msg);
        }

        match kind {
            "log" => {
                let level = msg.get_meta("level");
                let data = String::from_utf8_lossy(&msg.data);
                tracing::info!(chain_id = %self.chain_id, node_id = %self.node_id, level = %level, "{}", data);
                Result_ {
                    action: Action::Continue,
                    response: None,
                    error: None,
                    message: None,
                }
            }
            "config.get" => {
                let key = msg.get_meta("key");
                match self.config.get(key) {
                    Some(val) => Result_ {
                        action: Action::Respond,
                        response: Some(Response {
                            data: val.as_bytes().to_vec(),
                            meta: HashMap::new(),
                        }),
                        error: None,
                        message: None,
                    },
                    None => err_result(
                        ErrorCode::NOT_FOUND,
                        format!("config key not found: {key}"),
                    ),
                }
            }
            _ => err_result(
                ErrorCode::UNAVAILABLE,
                format!("unknown capability: {kind}"),
            ),
        }
    }

    fn capabilities(&self) -> Vec<CapabilityInfo> {
        vec![
            CapabilityInfo {
                kind: "call_block".to_string(),
                summary: "Call any block by name".to_string(),
                input: None,
                output: None,
            },
        ]
    }

    fn service(&self, name: &str) -> Option<&dyn Any> {
        self.named_services
            .get(name)
            .map(|s| s.as_ref() as &dyn Any)
    }

    fn services(&self) -> Option<&Services> {
        self.platform_services.as_deref()
    }
}
