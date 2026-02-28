// Request meta keys (set by bridge, read by blocks).
export const META_REQ_ACTION = 'req.action';
export const META_REQ_RESOURCE = 'req.resource';
export const META_REQ_PARAM_PREFIX = 'req.param.';
export const META_REQ_QUERY_PREFIX = 'req.query.';
export const META_REQ_CLIENT_IP = 'req.client.ip';
export const META_REQ_CONTENT_TYPE = 'req.content_type';

// Auth meta keys (set by auth infra block, read by blocks).
export const META_AUTH_USER_ID = 'auth.user_id';
export const META_AUTH_USER_EMAIL = 'auth.user_email';
export const META_AUTH_USER_ROLES = 'auth.user_roles';

// Response meta keys (set by blocks, read by bridge).
export const META_RESP_STATUS = 'resp.status';
export const META_RESP_CONTENT_TYPE = 'resp.content_type';
export const META_RESP_HEADER_PREFIX = 'resp.header.';
export const META_RESP_COOKIE_PREFIX = 'resp.set_cookie.';
