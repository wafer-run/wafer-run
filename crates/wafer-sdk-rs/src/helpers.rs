//! Helper functions and a response builder for common response patterns.

use crate::types::*;

// ---------------------------------------------------------------------------
// Free-standing helper functions
// ---------------------------------------------------------------------------

/// Build a response [`BlockResult`] with a status code, body, and content type.
pub fn respond(msg: Message, status: u16, data: Vec<u8>, content_type: &str) -> BlockResult {
    let mut meta = vec![
        MetaEntry { key: META_RESP_STATUS.to_string(), value: status.to_string() },
    ];
    if !content_type.is_empty() {
        meta.push(MetaEntry { key: META_RESP_CONTENT_TYPE.to_string(), value: content_type.to_string() });
    }
    msg.respond_with(Response { data, meta })
}

/// Serialize `data` as JSON and return a response with the given status code.
pub fn json_respond<T: serde::Serialize>(msg: Message, status: u16, data: &T) -> BlockResult {
    match serde_json::to_vec(data) {
        Ok(body) => respond(msg, status, body, "application/json"),
        Err(e) => error(msg, 500, ErrorCode::Internal, &e.to_string()),
    }
}

/// Return an error [`BlockResult`] with a status code, error code, and message.
pub fn error(msg: Message, status: u16, err_code: ErrorCode, err_message: &str) -> BlockResult {
    BlockResult {
        action: Action::Error,
        error: Some(WaferError {
            code: err_code,
            message: err_message.to_string(),
            meta: vec![MetaEntry { key: META_RESP_STATUS.to_string(), value: status.to_string() }],
        }),
        response: None,
        message: Some(msg),
    }
}

/// Return a 400 Bad Request error.
pub fn err_bad_request(msg: Message, message: &str) -> BlockResult {
    error(msg, 400, ErrorCode::InvalidArgument, message)
}

/// Return a 401 Unauthorized error.
pub fn err_unauthorized(msg: Message, message: &str) -> BlockResult {
    error(msg, 401, ErrorCode::Unauthenticated, message)
}

/// Return a 403 Forbidden error.
pub fn err_forbidden(msg: Message, message: &str) -> BlockResult {
    error(msg, 403, ErrorCode::PermissionDenied, message)
}

/// Return a 404 Not Found error.
pub fn err_not_found(msg: Message, message: &str) -> BlockResult {
    error(msg, 404, ErrorCode::NotFound, message)
}

/// Return a 409 Conflict error.
pub fn err_conflict(msg: Message, message: &str) -> BlockResult {
    error(msg, 409, ErrorCode::AlreadyExists, message)
}

/// Return a 422 Validation Error.
pub fn err_validation(msg: Message, message: &str) -> BlockResult {
    error(msg, 422, ErrorCode::InvalidArgument, message)
}

/// Return a 500 Internal Server Error.
pub fn err_internal(msg: Message, message: &str) -> BlockResult {
    error(msg, 500, ErrorCode::Internal, message)
}

// ---------------------------------------------------------------------------
// ResponseBuilder
// ---------------------------------------------------------------------------

/// A builder for constructing responses with headers, cookies, and status codes.
///
/// # Example
/// ```ignore
/// let result = ResponseBuilder::new(msg, 200)
///     .set_header("X-Request-Id", "abc123")
///     .set_cookie("session=xyz; HttpOnly; Path=/")
///     .json(&my_data);
/// ```
pub struct ResponseBuilder {
    msg: Message,
    #[allow(dead_code)]
    status: u16,
    meta: Vec<MetaEntry>,
    cookie_count: usize,
}

impl ResponseBuilder {
    /// Create a new response builder with the given message and HTTP status.
    pub fn new(msg: Message, status: u16) -> Self {
        let meta = vec![
            MetaEntry { key: META_RESP_STATUS.to_string(), value: status.to_string() },
        ];
        Self {
            msg,
            status,
            meta,
            cookie_count: 0,
        }
    }

    /// Add a `Set-Cookie` header to the response.
    pub fn set_cookie(mut self, cookie: &str) -> Self {
        self.meta.push(MetaEntry {
            key: format!("{}{}", META_RESP_COOKIE_PREFIX, self.cookie_count),
            value: cookie.to_string(),
        });
        self.cookie_count += 1;
        self
    }

    /// Add a response header.
    pub fn set_header(mut self, key: &str, value: &str) -> Self {
        self.meta.push(MetaEntry {
            key: format!("{}{}", META_RESP_HEADER_PREFIX, key),
            value: value.to_string(),
        });
        self
    }

    /// Serialize `data` as JSON and finalize the response.
    pub fn json<T: serde::Serialize>(mut self, data: &T) -> BlockResult {
        match serde_json::to_vec(data) {
            Ok(body) => {
                self.meta.push(MetaEntry {
                    key: META_RESP_CONTENT_TYPE.to_string(),
                    value: "application/json".to_string(),
                });
                self.msg.respond_with(Response {
                    data: body,
                    meta: self.meta,
                })
            }
            Err(e) => error(self.msg, 500, ErrorCode::Internal, &e.to_string()),
        }
    }

    /// Set a raw body with the given content type and finalize the response.
    pub fn body(mut self, data: Vec<u8>, content_type: &str) -> BlockResult {
        if !content_type.is_empty() {
            self.meta.push(MetaEntry {
                key: META_RESP_CONTENT_TYPE.to_string(),
                value: content_type.to_string(),
            });
        }
        self.msg.respond_with(Response {
            data,
            meta: self.meta,
        })
    }
}

/// Convenience constructor for [`ResponseBuilder`].
pub fn new_response(msg: Message, status: u16) -> ResponseBuilder {
    ResponseBuilder::new(msg, status)
}
