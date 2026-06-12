//! Typed client for the auth service.
//!
//! The auth block is owned by solobase (`suppers-ai/auth`) rather than
//! wafer-run.
//!
//! The three `require_*` ops carry their parameters in `Message::meta`
//! headers (`http.header.x-auth-scope`, `http.header.x-auth-role`) rather
//! than a request body — they go through `call_no_body`. The
//! `user_profile` op uses the standard buffered request/response shape.

use wafer_block::{
    wire::auth::{UserIdResponse, UserProfileRequest, UserProfileResponse},
    MetaEntry, ServiceOp, WaferError,
};

use super::common::{call, call_no_body};

const BLOCK: &str = "suppers-ai/auth";

const SCOPE_META_KEY: &str = "http.header.x-auth-scope";
const ROLE_META_KEY: &str = "http.header.x-auth-role";

/// Buffered: require an authenticated user on the current request.
///
/// Carries no request body — the host reads any required hints from the
/// in-flight request meta (cookies, bearer tokens, etc.). Returns the
/// resolved user id.
pub fn require_user() -> Result<UserIdResponse, WaferError> {
    call_no_body(BLOCK, ServiceOp::AUTH_REQUIRE_USER, vec![])
}

/// Buffered: require a token whose scope matches `scope`.
///
/// The scope is conveyed via the `http.header.x-auth-scope` meta header
/// (mirroring the host handler's `Message::header("x-auth-scope")` lookup).
/// No request body is sent. Returns the resolved user id.
pub fn require_token(scope: &str) -> Result<UserIdResponse, WaferError> {
    let meta = vec![MetaEntry {
        key: SCOPE_META_KEY.into(),
        value: scope.into(),
    }];
    call_no_body(BLOCK, ServiceOp::AUTH_REQUIRE_TOKEN, meta)
}

/// Buffered: require the current user to hold `role`.
///
/// The role is conveyed via the `http.header.x-auth-role` meta header
/// (mirroring the host handler's `Message::header("x-auth-role")` lookup).
/// No request body is sent. Returns the resolved user id.
pub fn require_role(role: &str) -> Result<UserIdResponse, WaferError> {
    let meta = vec![MetaEntry {
        key: ROLE_META_KEY.into(),
        value: role.into(),
    }];
    call_no_body(BLOCK, ServiceOp::AUTH_REQUIRE_ROLE, meta)
}

/// Buffered: fetch the full profile for a user id, including their
/// org memberships.
pub fn user_profile(request: &UserProfileRequest) -> Result<UserProfileResponse, WaferError> {
    call(BLOCK, ServiceOp::AUTH_USER_PROFILE, request)
}
