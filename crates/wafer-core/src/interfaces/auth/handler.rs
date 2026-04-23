//! Shared message handler logic for the auth block.
//!
//! Mirrors `crypto/handler.rs` shape. Dispatches `auth.require_user`,
//! `auth.require_token`, `auth.require_role` to the `AuthService` trait
//! supplied by consumers (solobase-core `blocks::auth::block::register`).
//!
//! The handler reads scope/role hints from request meta keys
//! `http.header.x-auth-scope` and `http.header.x-auth-role` (the same
//! convention `Message::header()` reads for ordinary HTTP headers).

use serde::{Deserialize, Serialize};
use wafer_block::{
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    *,
};

use super::service::{AuthError, AuthService, Role, TokenScope, UserId};
use crate::interfaces::handler_util::{decode_or_err, to_output};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserProfileRequest {
    user_id: String,
}

fn err_to_wafer(e: AuthError) -> WaferError {
    match e {
        AuthError::Unauthorized => WaferError::new(ErrorCode::UNAUTHENTICATED, "unauthorized"),
        AuthError::Forbidden => WaferError::new(ErrorCode::PERMISSION_DENIED, "forbidden"),
        AuthError::ProviderDown(m) => WaferError::new(ErrorCode::UNAVAILABLE, m),
        AuthError::NotFound => WaferError::new(ErrorCode::NOT_FOUND, "not found"),
        AuthError::Internal(m) => WaferError::new(ErrorCode::INTERNAL, m),
    }
}

#[derive(Serialize)]
struct UserIdResponse {
    user_id: String,
}

/// Handle an auth message by delegating to the given service.
pub async fn handle_message(service: &dyn AuthService, msg: &Message, body: &[u8]) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::AUTH_REQUIRE_USER => match service.require_user(msg).await {
            Ok(u) => to_output(&UserIdResponse { user_id: u.0 }),
            Err(e) => OutputStream::error(err_to_wafer(e)),
        },
        ServiceOp::AUTH_REQUIRE_TOKEN => {
            // Scope is carried via `x-auth-scope` header by convention; Plan
            // A2's server-side handlers set this explicitly before dispatch.
            let scope = match msg.header("x-auth-scope") {
                "" | "publish" => TokenScope::Publish,
                other => {
                    return OutputStream::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("unknown scope: {other}"),
                    ))
                }
            };
            match service.require_token(msg, scope).await {
                Ok(u) => to_output(&UserIdResponse { user_id: u.0 }),
                Err(e) => OutputStream::error(err_to_wafer(e)),
            }
        }
        ServiceOp::AUTH_REQUIRE_ROLE => {
            let role = match msg.header("x-auth-role") {
                "admin" => Role::Admin,
                _ => Role::User,
            };
            match service.require_role(msg, role).await {
                Ok(u) => to_output(&UserIdResponse { user_id: u.0 }),
                Err(e) => OutputStream::error(err_to_wafer(e)),
            }
        }
        ServiceOp::AUTH_USER_PROFILE => {
            let req = decode_or_err!(body, UserProfileRequest, "auth.user_profile");
            match service.user_profile(UserId(req.user_id)).await {
                Ok(p) => to_output(&p),
                Err(e) => OutputStream::error(err_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown auth op: {other}"),
        )),
    }
}
