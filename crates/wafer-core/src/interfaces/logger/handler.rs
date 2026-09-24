//! Shared message handler logic for the logger block.

use std::collections::HashMap;

use wafer_block::{
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    wire::logger as wire,
    *,
};

use super::service::{escape_log_text, Field, FieldValue, LoggerService};
use crate::interfaces::handler_util::decode_or_err;

// --- Helpers ---

/// Convert the request's JSON fields to service fields, escaping every key
/// and every text value with [`escape_log_text`]. Fields are ordered by key,
/// so a record renders the same way on every call.
fn json_fields_to_service_fields(fields: &HashMap<String, serde_json::Value>) -> Vec<Field> {
    let mut out: Vec<Field> = fields
        .iter()
        .map(|(k, v)| Field {
            key: escape_log_text(k),
            value: match v {
                serde_json::Value::String(s) => FieldValue::String(escape_log_text(s)),
                serde_json::Value::Number(n) => n.as_i64().map_or_else(
                    || {
                        n.as_f64()
                            .map_or_else(|| FieldValue::Any(n.to_string()), FieldValue::Float)
                    },
                    FieldValue::Int,
                ),
                serde_json::Value::Bool(b) => FieldValue::Bool(*b),
                // Compact JSON escapes control characters inside its strings,
                // but not the U+2028 / U+2029 separators.
                other => FieldValue::Any(escape_log_text(&other.to_string())),
            },
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Handle a logger message by delegating to the given service.
///
/// All four verbs share `wire::LogRequest`; only `Message::kind` distinguishes
/// the level. The record handed to the service carries `ctx.caller_id()` —
/// the runtime's name for the block that sent it, never a name the block
/// wrote — and the message and field texts escaped with [`escape_log_text`].
/// Logger calls are fire-and-forget — return an empty
/// `OutputStream::respond(vec![])`.
pub fn handle_message(
    service: &dyn LoggerService,
    ctx: &dyn Context,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    let req = decode_or_err!(body, wire::LogRequest, "logger.log");
    let caller = ctx.caller_id();
    let log_msg = escape_log_text(&req.message);
    let fields = json_fields_to_service_fields(&req.fields);

    match msg.kind.as_str() {
        ServiceOp::LOGGER_DEBUG => {
            service.debug(caller, &log_msg, &fields);
            OutputStream::respond(vec![])
        }
        ServiceOp::LOGGER_INFO => {
            service.info(caller, &log_msg, &fields);
            OutputStream::respond(vec![])
        }
        ServiceOp::LOGGER_WARN => {
            service.warn(caller, &log_msg, &fields);
            OutputStream::respond(vec![])
        }
        ServiceOp::LOGGER_ERROR => {
            service.error(caller, &log_msg, &fields);
            OutputStream::respond(vec![])
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("unknown logger operation: {other}"),
        )),
    }
}
