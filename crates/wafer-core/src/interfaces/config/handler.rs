//! Shared message handler logic for the config block.
//!
//! Access control is enforced by WRAP in `call_block()` — the config client
//! sets `wrap.resource` to the config key, and the runtime checks ownership
//! and grants before dispatching here. This handler is pure business logic.

use serde::{Deserialize, Serialize};

use wafer_block::common::{ErrorCode, ServiceOp};
use wafer_block::helpers::{respond_empty, respond_json};
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

/// Handle a config message by delegating to the given service.
///
/// Access control is handled by WRAP in `call_block()` before this is called.
pub fn handle_message(service: &dyn ConfigService, msg: &mut Message) -> Result_ {
    match msg.kind.as_str() {
        ServiceOp::CONFIG_GET => {
            let key = match msg.decode::<GetRequest>() {
                Ok(req) => req.key,
                Err(_) => {
                    let meta_key = msg.get_meta("key");
                    if meta_key.is_empty() {
                        return Result_::error(WaferError::new(
                            ErrorCode::INVALID_ARGUMENT,
                            "config.get requires a 'key' in data or meta",
                        ));
                    }
                    meta_key.to_string()
                }
            };

            match service.get(&key) {
                Some(val) => respond_json(msg, &GetResponse { value: val }),
                None => Result_::error(WaferError::new(
                    ErrorCode::NOT_FOUND,
                    format!("config key not found: {key}"),
                )),
            }
        }
        ServiceOp::CONFIG_SET => {
            let req: SetRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid config.set request: {e}"),
                    ))
                }
            };

            service.set(&req.key, &req.value);
            respond_empty(msg)
        }
        other => Result_::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown config operation: {other}"),
        )),
    }
}
