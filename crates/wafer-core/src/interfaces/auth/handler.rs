//! Shared message handler logic for the auth block.
//!
//! Mirrors `crypto/handler.rs` shape. Dispatches `auth.require_user`,
//! `auth.require_token`, `auth.require_role` to the `AuthService` trait
//! supplied by consumers (the consuming application's `blocks::auth::block::register`).
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
        AuthError::Unauthorized => WaferError::new(ErrorCode::Unauthenticated, "unauthorized"),
        AuthError::Forbidden => WaferError::new(ErrorCode::PermissionDenied, "forbidden"),
        AuthError::ProviderDown(m) => WaferError::new(ErrorCode::Unavailable, m),
        AuthError::NotFound => WaferError::new(ErrorCode::NotFound, "not found"),
        AuthError::Internal(m) => WaferError::new(ErrorCode::Internal, m),
    }
}

fn service_role_to_wire(r: &Role) -> String {
    match r {
        Role::User => "user".to_string(),
        Role::Admin => "admin".to_string(),
    }
}

/// Parse the `x-auth-role` header value into a [`Role`], case-insensitively.
///
/// Fails closed: an unrecognized value returns `InvalidArgument` rather than
/// silently defaulting to `Role::User` (the weakest role) — mirrors the
/// `x-auth-scope` handling in `AUTH_REQUIRE_TOKEN` below. Accepts the same
/// lowercase spelling `service_role_to_wire` emits on the response side, so a
/// caller can round-trip a `profile.role` value straight back into this
/// header without it silently downgrading to `Role::User`.
fn parse_required_role(header: &str) -> Result<Role, WaferError> {
    match header.to_ascii_lowercase().as_str() {
        "admin" => Ok(Role::Admin),
        "user" => Ok(Role::User),
        _ => Err(WaferError::new(
            ErrorCode::InvalidArgument,
            format!("unknown role: {header}"),
        )),
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
                        ErrorCode::InvalidArgument,
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
            let role = match parse_required_role(msg.header("x-auth-role")) {
                Ok(r) => r,
                Err(e) => return OutputStream::error(e),
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
            ErrorCode::Unimplemented,
            format!("unknown auth op: {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_role_rejects_unknown_role_header_instead_of_defaulting_to_user() {
        // Silently mapping any non-"admin" header to Role::User means a caller
        // passing an unrecognized value gets the weakest check — fail-open.
        let err = parse_required_role("Superuser")
            .expect_err("unknown role must be InvalidArgument, not Role::User");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn require_role_is_case_insensitive_for_admin() {
        assert_eq!(parse_required_role("Admin").unwrap(), Role::Admin);
        assert_eq!(parse_required_role("admin").unwrap(), Role::Admin);
        assert_eq!(parse_required_role("ADMIN").unwrap(), Role::Admin);
    }

    #[test]
    fn require_role_is_case_insensitive_for_user() {
        assert_eq!(parse_required_role("User").unwrap(), Role::User);
        assert_eq!(parse_required_role("user").unwrap(), Role::User);
    }

    #[test]
    fn require_role_rejects_empty_header() {
        // An absent `x-auth-role` header resolves to `""` via
        // `Message::header`, which must not be treated as any role.
        let err = parse_required_role("")
            .expect_err("missing role header must be InvalidArgument, not a default role");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn service_role_to_wire_uses_canonical_lowercase() {
        // The request-header format (`parse_required_role`) and the
        // response-profile format (`service_role_to_wire`) must agree on one
        // casing so `profile.role` round-trips straight back into
        // `require_role` without failing open.
        assert_eq!(service_role_to_wire(&Role::Admin), "admin");
        assert_eq!(service_role_to_wire(&Role::User), "user");
        assert_eq!(
            parse_required_role(&service_role_to_wire(&Role::Admin)).unwrap(),
            Role::Admin
        );
        assert_eq!(
            parse_required_role(&service_role_to_wire(&Role::User)).unwrap(),
            Role::User
        );
    }
}
