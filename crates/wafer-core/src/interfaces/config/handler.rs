//! Shared message handler logic for the config block.
//!
//! `ctx` is the trusted host-side authorization surface: both ops authorize
//! via [`decode_and_authorize`], which bundles the codec decode with a call
//! to `ctx.check_resource_access`, so an arm cannot obtain its typed request
//! without also being checked. The key is read from the codec-encoded body
//! only; a body that does not decode is `InvalidArgument`.

use wafer_block::{
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
            let req = match decode_and_authorize::<wire::GetRequest>(ctx, body, "config.get", |r| {
                (r.key.clone(), ResourceType::Config, ResourceAccess::Read)
            }) {
                Ok(r) => r,
                Err(out) => return out,
            };
            service.get(&req.key).map_or_else(
                || {
                    OutputStream::error(WaferError::new(
                        ErrorCode::NotFound,
                        format!("config key not found: {}", req.key),
                    ))
                },
                |val| to_output(&wire::GetResponse { value: val }),
            )
        }
        ServiceOp::CONFIG_SET => {
            let req = match decode_and_authorize::<wire::SetRequest>(ctx, body, "config.set", |r| {
                (r.key.clone(), ResourceType::Config, ResourceAccess::Write)
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
