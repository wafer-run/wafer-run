//! Typed client for the auth service.
//!
//! The auth block is owned by solobase (`suppers-ai/auth`) rather than
//! wafer-run.
//!
//! The three `require_*` ops carry their parameters in `Message::meta`
//! headers (`http.header.x-auth-scope`, `http.header.x-auth-role`) rather
//! than a request body — see
//! [`crate::clients::common::open_no_body_with_meta`]. The
//! `user_profile` op uses the standard buffered request/response shape.

use wafer_block::{
    codec,
    wire::auth::{UserIdResponse, UserProfileRequest, UserProfileResponse},
    MetaEntry, ServiceOp, WaferError,
};

use super::common::{collect_single_frame, open_buffered, open_no_body_with_meta};

const BLOCK: &str = "suppers-ai/auth";

/// Buffered: require an authenticated user on the current request.
///
/// Carries no request body — the host reads any required hints from the
/// in-flight request meta (cookies, bearer tokens, etc.). Returns the
/// resolved user id.
pub fn require_user() -> Result<UserIdResponse, WaferError> {
    let mut response_stream = open_no_body_with_meta(BLOCK, ServiceOp::AUTH_REQUIRE_USER, vec![])?;
    let body = collect_single_frame(&mut response_stream, "auth REQUIRE_USER")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding auth REQUIRE_USER response: {}", e.message),
        )
    })
}

/// Buffered: require a token whose scope matches `scope`.
///
/// The scope is conveyed via the `http.header.x-auth-scope` meta header
/// (mirroring the host handler's `Message::header("x-auth-scope")` lookup).
/// No request body is sent. Returns the resolved user id.
pub fn require_token(scope: &str) -> Result<UserIdResponse, WaferError> {
    let meta = vec![MetaEntry {
        key: "http.header.x-auth-scope".into(),
        value: scope.into(),
    }];
    let mut response_stream = open_no_body_with_meta(BLOCK, ServiceOp::AUTH_REQUIRE_TOKEN, meta)?;
    let body = collect_single_frame(&mut response_stream, "auth REQUIRE_TOKEN")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding auth REQUIRE_TOKEN response: {}", e.message),
        )
    })
}

/// Buffered: require the current user to hold `role`.
///
/// The role is conveyed via the `http.header.x-auth-role` meta header
/// (mirroring the host handler's `Message::header("x-auth-role")` lookup).
/// No request body is sent. Returns the resolved user id.
pub fn require_role(role: &str) -> Result<UserIdResponse, WaferError> {
    let meta = vec![MetaEntry {
        key: "http.header.x-auth-role".into(),
        value: role.into(),
    }];
    let mut response_stream = open_no_body_with_meta(BLOCK, ServiceOp::AUTH_REQUIRE_ROLE, meta)?;
    let body = collect_single_frame(&mut response_stream, "auth REQUIRE_ROLE")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding auth REQUIRE_ROLE response: {}", e.message),
        )
    })
}

/// Buffered: fetch the full profile for a user id, including their
/// org memberships.
pub fn user_profile(request: &UserProfileRequest) -> Result<UserProfileResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::AUTH_USER_PROFILE, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "auth USER_PROFILE")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding auth USER_PROFILE response: {}", e.message),
        )
    })
}
