//! Shared message handler logic for the logger block.

use std::collections::HashMap;

use serde::Deserialize;

use wafer_block::common::{ErrorCode, ServiceOp};
use wafer_block::*;

use super::service::{Field, FieldValue, LoggerService};

// --- Request types ---

#[derive(Deserialize)]
struct LogRequest {
    #[serde(default)]
    message: String,
    #[serde(default)]
    fields: HashMap<String, serde_json::Value>,
}

// --- Helpers ---

fn json_fields_to_service_fields(fields: &HashMap<String, serde_json::Value>) -> Vec<Field> {
    fields
        .iter()
        .map(|(k, v)| Field {
            key: k.clone(),
            value: match v {
                serde_json::Value::String(s) => FieldValue::String(s.clone()),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        FieldValue::Int(i)
                    } else if let Some(f) = n.as_f64() {
                        FieldValue::Float(f)
                    } else {
                        FieldValue::Any(n.to_string())
                    }
                }
                serde_json::Value::Bool(b) => FieldValue::Bool(*b),
                other => FieldValue::Any(other.to_string()),
            },
        })
        .collect()
}

fn respond_ok(msg: &Message) -> Result_ {
    Result_ {
        action: Action::Continue,
        response: None,
        error: None,
        message: Some(msg.clone()),
    }
}

/// Handle a logger message by delegating to the given service.
pub fn handle_message(service: &dyn LoggerService, msg: &mut Message) -> Result_ {
    let (log_msg, fields) = match msg.decode::<LogRequest>() {
        Ok(req) => (req.message, json_fields_to_service_fields(&req.fields)),
        Err(_) => {
            let text = String::from_utf8_lossy(&msg.data).to_string();
            (text, Vec::new())
        }
    };

    match msg.kind.as_str() {
        ServiceOp::LOGGER_DEBUG => {
            service.debug(&log_msg, &fields);
            respond_ok(msg)
        }
        ServiceOp::LOGGER_INFO => {
            service.info(&log_msg, &fields);
            respond_ok(msg)
        }
        ServiceOp::LOGGER_WARN => {
            service.warn(&log_msg, &fields);
            respond_ok(msg)
        }
        ServiceOp::LOGGER_ERROR => {
            service.error(&log_msg, &fields);
            respond_ok(msg)
        }
        other => Result_::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown logger operation: {other}"),
        )),
    }
}
