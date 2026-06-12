//! Well-known `MetaEntry.key` constants exchanged between the runtime/bridge
//! and blocks (request inputs, auth context, WRAP capability tags, response
//! outputs). These string keys form the contract between hosts and blocks for
//! out-of-band attributes that don't belong on the message body.

// Request meta keys (set by bridge, read by blocks).

/// Request action verb (e.g. `retrieve`, `create`).
pub const META_REQ_ACTION: &str = "req.action";
/// Request resource path (e.g. `/orgs/123`).
pub const META_REQ_RESOURCE: &str = "req.resource";
/// Prefix for URL path variables (`req.param.{name}`).
pub const META_REQ_PARAM_PREFIX: &str = "req.param.";
/// Prefix for query string parameters (`req.query.{name}`).
pub const META_REQ_QUERY_PREFIX: &str = "req.query.";
/// Client IP address as observed by the bridge.
pub const META_REQ_CLIENT_IP: &str = "req.client.ip";
/// Request `Content-Type` header value.
pub const META_REQ_CONTENT_TYPE: &str = "req.content_type";

// Auth meta keys (set by auth infra block, read by blocks).

/// Authenticated user identifier.
pub const META_AUTH_USER_ID: &str = "auth.user_id";
/// Authenticated user email address.
pub const META_AUTH_USER_EMAIL: &str = "auth.user_email";
/// Comma-separated list of roles for the authenticated user.
pub const META_AUTH_USER_ROLES: &str = "auth.user_roles";

// Trace correlation (set by the bridge/listener, read by observability hooks).

/// Distributed-trace correlation id carried on the message meta and copied
/// into [`crate`]-level observability contexts.
pub const META_TRACE_ID: &str = "trace_id";

// WRAP meta keys (set by client wrappers, read by runtime in call_block()).

/// Name of the resource being accessed (table/key/object name).
pub const META_WRAP_RESOURCE: &str = "wrap.resource";
/// Access mode for the WRAP check — `"read"` or `"write"`.
pub const META_WRAP_ACCESS: &str = "wrap.access";
/// Resource type for the WRAP check — `"db"`, `"config"`, `"storage"`,
/// `"crypto"`, or `"network"`.
pub const META_WRAP_RESOURCE_TYPE: &str = "wrap.resource_type";

// Error meta keys (set by blocks on `WaferError.meta`, read by adapters).

/// Precise, machine-readable application error code carried as structured
/// meta on a [`crate::WaferError`] (e.g. `auth.invalid_token`). The coarse
/// [`crate::ErrorCode`] classifies the failure for transport/status mapping;
/// this key carries the app-level code so consumers don't have to smuggle it
/// into the human-readable message. Set via
/// [`crate::WaferError::with_detail_code`], read via
/// [`crate::WaferError::detail_code`]; HTTP adapters surface it as a JSON
/// `code` field on error responses.
pub const META_ERROR_CODE: &str = "error.code";

// Response meta keys (set by blocks, read by bridge).

/// Response HTTP status code (stringified integer).
pub const META_RESP_STATUS: &str = "resp.status";
/// Response `Content-Type` header value.
pub const META_RESP_CONTENT_TYPE: &str = "resp.content_type";
/// Prefix for response headers (`resp.header.{name}`).
pub const META_RESP_HEADER_PREFIX: &str = "resp.header.";
/// Prefix for response `Set-Cookie` directives (`resp.set_cookie.{name}`).
pub const META_RESP_COOKIE_PREFIX: &str = "resp.set_cookie.";
