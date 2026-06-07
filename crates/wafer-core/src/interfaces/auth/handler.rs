//! Shared message handler logic for the auth block.
//!
//! Mirrors `crypto/handler.rs` shape. Dispatches `auth.require_user`,
//! `auth.require_token`, `auth.require_role` to the `AuthService` trait
//! supplied by consumers (solobase-core `blocks::auth::block::register`).
//!
//! The handler reads scope/role hints from request meta keys
//! `http.header.x-auth-scope` and `http.header.x-auth-role` (the same
//! convention `Message::header()` reads for ordinary HTTP headers).

use wafer_block::{
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    wire::auth as wire,
    *,
};

use super::service::{self, AuthError, AuthService, Role, TokenScope, UserId};
use crate::interfaces::handler_util::{decode_or_err, to_output};

fn err_to_wafer(e: AuthError) -> WaferError {
    match e {
        AuthError::Unauthorized => WaferError::new(ErrorCode::UNAUTHENTICATED, "unauthorized"),
        AuthError::Forbidden => WaferError::new(ErrorCode::PERMISSION_DENIED, "forbidden"),
        AuthError::ProviderDown(m) => WaferError::new(ErrorCode::UNAVAILABLE, m),
        AuthError::NotFound => WaferError::new(ErrorCode::NOT_FOUND, "not found"),
        AuthError::Internal(m) => WaferError::new(ErrorCode::INTERNAL, m),
    }
}

fn service_role_to_wire(r: &Role) -> String {
    match r {
        Role::User => "User".to_string(),
        Role::Admin => "Admin".to_string(),
    }
}

fn service_org_to_wire(o: service::OrgSummary) -> wire::OrgSummary {
    wire::OrgSummary {
        name: o.name,
        verified_via: o.verified_via,
        verified_ref: o.verified_ref,
        is_reserved: o.is_reserved,
    }
}

fn service_profile_to_wire(p: service::UserProfile) -> wire::UserProfileResponse {
    wire::UserProfileResponse {
        id: p.id.0,
        email: p.email,
        display_name: p.display_name,
        avatar_url: p.avatar_url,
        role: service_role_to_wire(&p.role),
        orgs: p.orgs.into_iter().map(service_org_to_wire).collect(),
    }
}

/// Handle an auth message by delegating to the given service.
pub async fn handle_message(service: &dyn AuthService, msg: &Message, body: &[u8]) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::AUTH_REQUIRE_USER => match service.require_user(msg).await {
            Ok(u) => to_output(&wire::UserIdResponse { user_id: u.0 }),
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
                Ok(u) => to_output(&wire::UserIdResponse { user_id: u.0 }),
                Err(e) => OutputStream::error(err_to_wafer(e)),
            }
        }
        ServiceOp::AUTH_REQUIRE_ROLE => {
            let role = match msg.header("x-auth-role") {
                "admin" => Role::Admin,
                _ => Role::User,
            };
            match service.require_role(msg, role).await {
                Ok(u) => to_output(&wire::UserIdResponse { user_id: u.0 }),
                Err(e) => OutputStream::error(err_to_wafer(e)),
            }
        }
        ServiceOp::AUTH_USER_PROFILE => {
            let req = decode_or_err!(body, wire::UserProfileRequest, "auth.user_profile");
            match service.user_profile(UserId(req.user_id)).await {
                Ok(p) => to_output(service_profile_to_wire(p)),
                Err(e) => OutputStream::error(err_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown auth op: {other}"),
        )),
    }
}
