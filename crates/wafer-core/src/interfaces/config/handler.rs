//! Shared message handler logic for the config block.
//!
//! Access control is enforced by WRAP in `call_block()` — the config client
//! sets `wrap.resource` to the config key, and the runtime checks ownership
//! and grants before dispatching here. This handler is pure business logic.

use serde::{Deserialize, Serialize};

use wafer_block::common::{ErrorCode, ServiceOp};
use wafer_block::streams::output::OutputStream;
use wafer_block::*;

use super::service::ConfigService;

#[derive(Deserialize)]
struct GetRequest {
    key: String,
}

#[derive(Deserialize)]
struct SetRequest {
    key: String,
    value: String,
}

#[derive(Serialize)]
struct GetResponse {
    value: String,
}

fn to_output<T: serde::Serialize>(val: T) -> OutputStream {
    match serde_json::to_vec(&val) {
        Ok(bytes) => OutputStream::respond(bytes),
        Err(e) => OutputStream::error(WaferError::new(
            ErrorCode::INTERNAL,
            format!("serialize response: {e}"),
        )),
    }
}

/// Handle a config message by delegating to the given service.
///
/// Access control is handled by WRAP in `call_block()` before this is called.
pub fn handle_message(service: &dyn ConfigService, msg: &Message, body: &[u8]) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::CONFIG_GET => {
            let key = match serde_json::from_slice::<GetRequest>(body) {
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
                Some(val) => to_output(&GetResponse { value: val }),
                None => OutputStream::error(WaferError::new(
                    ErrorCode::NOT_FOUND,
                    format!("config key not found: {key}"),
                )),
            }
        }
        ServiceOp::CONFIG_SET => {
            let req: SetRequest = match serde_json::from_slice(body) {
                Ok(r) => r,
                Err(e) => {
                    return OutputStream::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid config.set request: {e}"),
                    ))
                }
            };

            service.set(&req.key, &req.value);
            OutputStream::respond(vec![])
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown config operation: {other}"),
        )),
    }
}
