use std::collections::HashMap;

use serde::Deserialize;

use crate::block::{Block, BlockInfo};
use crate::common::{ErrorCode, ServiceOp};
use crate::context::Context;
use crate::types::*;

/// LoggerBlock provides structured logging as a Block.
/// Uses the tracing crate directly; no service dependency required.
pub struct LoggerBlock;

impl LoggerBlock {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LoggerBlock {
    fn default() -> Self {
        Self::new()
    }
}

// --- Request types ---

#[derive(Deserialize)]
struct LogRequest {
    #[serde(default)]
    message: String,
    #[serde(default)]
    fields: HashMap<String, serde_json::Value>,
}

// --- Helpers ---

fn format_fields(fields: &HashMap<String, serde_json::Value>) -> String {
    if fields.is_empty() {
        return String::new();
    }
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join(" ")
}

fn respond_ok(msg: &Message) -> Result_ {
    Result_ {
        action: Action::Continue,
        response: None,
        error: None,
        message: Some(msg.clone()),
    }
}

impl Block for LoggerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer/logger".to_string(),
            version: "0.1.0".to_string(),
            interface: "wafer.infra.logger".to_string(),
            summary: "Structured logging via tracing".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }

    fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        // Parse the log message from the data payload.
        // If parsing fails, use the raw data bytes as the message string.
        let (log_msg, fields_str) = match msg.decode::<LogRequest>() {
            Ok(req) => (req.message, format_fields(&req.fields)),
            Err(_) => {
                // Fallback: treat the entire data as a plain-text log message
                let text = String::from_utf8_lossy(&msg.data).to_string();
                (text, String::new())
            }
        };

        match msg.kind.as_str() {
            ServiceOp::LOGGER_DEBUG => {
                if fields_str.is_empty() {
                    tracing::debug!("{}", log_msg);
                } else {
                    tracing::debug!("{} {}", log_msg, fields_str);
                }
                respond_ok(msg)
            }
            ServiceOp::LOGGER_INFO => {
                if fields_str.is_empty() {
                    tracing::info!("{}", log_msg);
                } else {
                    tracing::info!("{} {}", log_msg, fields_str);
                }
                respond_ok(msg)
            }
            ServiceOp::LOGGER_WARN => {
                if fields_str.is_empty() {
                    tracing::warn!("{}", log_msg);
                } else {
                    tracing::warn!("{} {}", log_msg, fields_str);
                }
                respond_ok(msg)
            }
            ServiceOp::LOGGER_ERROR => {
                if fields_str.is_empty() {
                    tracing::error!("{}", log_msg);
                } else {
                    tracing::error!("{} {}", log_msg, fields_str);
                }
                respond_ok(msg)
            }
            other => Result_::error(WaferError::new(
                ErrorCode::UNIMPLEMENTED,
                format!("unknown logger operation: {other}"),
            )),
        }
    }

    fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}
