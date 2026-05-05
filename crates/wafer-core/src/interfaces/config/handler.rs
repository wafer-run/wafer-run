//! Shared message handler logic for the config block.
//!
//! Access control is enforced by WRAP in `call_block()` — the config client
//! sets `wrap.resource` to the config key, and the runtime checks ownership
//! and grants before dispatching here. This handler is pure business logic.

use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    wire::config as wire,
    *,
};

use super::service::ConfigService;
use crate::interfaces::handler_util::{decode_or_err, to_output};

/// Handle a config message by delegating to the given service.
///
/// Access control is handled by WRAP in `call_block()` before this is called.
pub fn handle_message(service: &dyn ConfigService, msg: &Message, body: &[u8]) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::CONFIG_GET => {
            // Accept either a codec-encoded `GetRequest` body or a `key` meta
            // field on the message (a fallback for callers that route through
            // headers — preserves the original handler's behavior).
            let key = match codec::decode::<wire::GetRequest>(body) {
                Ok(req) => req.key,
                Err(_) => {
                    let meta_key = msg.get_meta("key");
                    if meta_key.is_empty() {
                        return OutputStream::error(WaferError::new(
                            ErrorCode::INVALID_ARGUMENT,
                            "config.get requires a 'key' in data or meta",
                        ));
                    }
                    meta_key.to_string()
                }
            };

            match service.get(&key) {
                Some(val) => to_output(&wire::GetResponse { value: val }),
                None => OutputStream::error(WaferError::new(
                    ErrorCode::NOT_FOUND,
                    format!("config key not found: {key}"),
                )),
            }
        }
        ServiceOp::CONFIG_SET => {
            let req = decode_or_err!(body, wire::SetRequest, "config.set");
            service.set(&req.key, &req.value);
            OutputStream::respond(vec![])
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown config operation: {other}"),
        )),
    }
}
