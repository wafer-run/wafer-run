// Meta key constants exposed by the JS/TS SDK.
//
// Hand-maintained. The canonical definitions are the Rust constants in
// crates/wafer-block/src/meta.rs — names and values here must mirror that
// file exactly. When adding a key, copy the name/value from the Rust side.

/** Action being performed (e.g. "create", "list"). */
export const META_REQ_ACTION = 'req.action';
/** Resource the action targets (e.g. "users"). */
export const META_REQ_RESOURCE = 'req.resource';
/** Prefix for path parameters (e.g. "req.param.id"). */
export const META_REQ_PARAM_PREFIX = 'req.param.';
/** Prefix for query-string parameters (e.g. "req.query.page"). */
export const META_REQ_QUERY_PREFIX = 'req.query.';
/** Client IP address of the originating request. */
export const META_REQ_CLIENT_IP = 'req.client.ip';
/** Content-Type of the request body. */
export const META_REQ_CONTENT_TYPE = 'req.content_type';
/** Authenticated user id. */
export const META_AUTH_USER_ID = 'auth.user_id';
/** Authenticated user email. */
export const META_AUTH_USER_EMAIL = 'auth.user_email';
/** Comma-separated authenticated user roles. */
export const META_AUTH_USER_ROLES = 'auth.user_roles';
/** HTTP status code to respond with. */
export const META_RESP_STATUS = 'resp.status';
/** Content-Type of the response body. */
export const META_RESP_CONTENT_TYPE = 'resp.content_type';
/** Prefix for response headers (e.g. "resp.header.cache-control"). */
export const META_RESP_HEADER_PREFIX = 'resp.header.';
/** Prefix for response Set-Cookie values (e.g. "resp.set_cookie.session"). */
export const META_RESP_COOKIE_PREFIX = 'resp.set_cookie.';
