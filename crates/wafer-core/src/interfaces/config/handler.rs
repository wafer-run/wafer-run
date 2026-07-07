//! Shared message handler logic for the config block.
//!
//! `ctx` is the trusted host-side authorization surface: `CONFIG_SET`
//! authorizes via [`decode_and_authorize`], which bundles the codec decode
//! with a call to `ctx.check_resource_access` so the arm cannot obtain its
//! typed request without also being checked. `CONFIG_GET` accepts a key via
//! either a codec-encoded body or a `key` meta fallback (see below), so it
//! cannot use the single-decode helper directly; it calls
//! `ctx.check_resource_access` itself immediately after resolving the key
//! and before it is ever handed to the service.

use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    types::ResourceType,
    wire::config as wire,
    *,
};

use super::service::ConfigService;
use crate::interfaces::handler_util::{decode_and_authorize, to_output};

/// Handle a config message by delegating to the given service.
pub fn handle_message(
    service: &dyn ConfigService,
    ctx: &dyn Context,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::CONFIG_GET => {
            // Accept either a codec-encoded `GetRequest` body or a `key` meta
            // field on the message (a fallback for callers that route through
            // headers — preserves the original handler's behavior). Because
            // the key can come from either source, this arm can't use
            // `decode_and_authorize`'s single-decode bundling; it authorizes
            // manually right after resolving `key`, before calling the
            // service.
            let key = match codec::decode::<wire::GetRequest>(body) {
                Ok(req) => req.key,
                Err(_) => {
                    let meta_key = msg.get_meta("key");
                    if meta_key.is_empty() {
                        return OutputStream::error(WaferError::new(
                            ErrorCode::InvalidArgument,
                            "config.get requires a 'key' in data or meta",
                        ));
                    }
                    meta_key.to_string()
                }
            };

            if let Err(e) = ctx.check_resource_access(&key, ResourceType::Config, false) {
                return OutputStream::error(e);
            }
            service.get(&key).map_or_else(
                || {
                    OutputStream::error(WaferError::new(
                        ErrorCode::NotFound,
                        format!("config key not found: {key}"),
                    ))
                },
                |val| to_output(&wire::GetResponse { value: val }),
            )
        }
        ServiceOp::CONFIG_SET => {
            let req = match decode_and_authorize::<wire::SetRequest>(ctx, body, "config.set", |r| {
                (r.key.clone(), ResourceType::Config, true)
            }) {
                Ok(r) => r,
                Err(out) => return out,
            };
            service.set(&req.key, &req.value);
            OutputStream::respond(vec![])
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("unknown config operation: {other}"),
        )),
    }
}
