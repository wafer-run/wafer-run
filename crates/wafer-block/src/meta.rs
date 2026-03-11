// Request meta keys (set by bridge, read by blocks).
pub const META_REQ_ACTION: &str = "req.action";
pub const META_REQ_RESOURCE: &str = "req.resource";
pub const META_REQ_PARAM_PREFIX: &str = "req.param.";
pub const META_REQ_QUERY_PREFIX: &str = "req.query.";
pub const META_REQ_CLIENT_IP: &str = "req.client.ip";
pub const META_REQ_CONTENT_TYPE: &str = "req.content_type";

// Auth meta keys (set by auth infra block, read by blocks).
pub const META_AUTH_USER_ID: &str = "auth.user_id";
pub const META_AUTH_USER_EMAIL: &str = "auth.user_email";
pub const META_AUTH_USER_ROLES: &str = "auth.user_roles";

// Response meta keys (set by blocks, read by bridge).
pub const META_RESP_STATUS: &str = "resp.status";
pub const META_RESP_CONTENT_TYPE: &str = "resp.content_type";
pub const META_RESP_HEADER_PREFIX: &str = "resp.header.";
pub const META_RESP_COOKIE_PREFIX: &str = "resp.set_cookie.";
