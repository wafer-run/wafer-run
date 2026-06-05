//! Shared message handler logic for the logger block.

use std::collections::HashMap;

use wafer_block::{
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    wire::logger as wire,
    *,
};

use super::service::{Field, FieldValue, LoggerService};
use crate::interfaces::handler_util::decode_or_err;

// --- Helpers ---

fn json_fields_to_service_fields(fields: &HashMap<String, serde_json::Value>) -> Vec<Field> {
    fields
        .iter()
        .map(|(k, v)| Field {
            key: k.clone(),
            value: match v {
                serde_json::Value::String(s) => FieldValue::String(s.clone()),
                serde_json::Value::Number(n) => n.as_i64().map_or_else(
                    || {
                        n.as_f64()
                            .map_or_else(|| FieldValue::Any(n.to_string()), FieldValue::Float)
                    },
                    FieldValue::Int,
                ),
                serde_json::Value::Bool(b) => FieldValue::Bool(*b),
                other => FieldValue::Any(other.to_string()),
            },
        })
        .collect()
}

/// Handle a logger message by delegating to the given service.
///
/// All four verbs share `wire::LogRequest`; only `Message::kind` distinguishes
/// the level. Logger calls are fire-and-forget — return an empty
/// `OutputStream::respond(vec![])`.
pub fn handle_message(service: &dyn LoggerService, msg: &Message, body: &[u8]) -> OutputStream {
    let req = decode_or_err!(body, wire::LogRequest, "logger.log");
    let log_msg = req.message;
    let fields = json_fields_to_service_fields(&req.fields);

    match msg.kind.as_str() {
        ServiceOp::LOGGER_DEBUG => {
            service.debug(&log_msg, &fields);
            OutputStream::respond(vec![])
        }
        ServiceOp::LOGGER_INFO => {
            service.info(&log_msg, &fields);
            OutputStream::respond(vec![])
        }
        ServiceOp::LOGGER_WARN => {
            service.warn(&log_msg, &fields);
            OutputStream::respond(vec![])
        }
        ServiceOp::LOGGER_ERROR => {
            service.error(&log_msg, &fields);
            OutputStream::respond(vec![])
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown logger operation: {other}"),
        )),
    }
}
