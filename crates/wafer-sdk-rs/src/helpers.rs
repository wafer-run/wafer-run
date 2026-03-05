//! Helper functions and a response builder for common response patterns.

use crate::types::*;

// ---------------------------------------------------------------------------
// Free-standing helper functions
// ---------------------------------------------------------------------------

/// Build a response [`BlockResult`] with a body and content type.
/// The transport layer determines the status code (default 200 for Respond).
pub fn respond(msg: Message, data: Vec<u8>, content_type: &str) -> BlockResult {
    let mut meta = Vec::new();
    if !content_type.is_empty() {
        meta.push(MetaEntry { key: META_RESP_CONTENT_TYPE.to_string(), value: content_type.to_string() });
    }
    msg.respond_with(Response { data, meta })
}

/// Serialize `data` as JSON and return a response.
/// The transport layer determines the status code.
pub fn json_respond<T: serde::Serialize>(msg: Message, data: &T) -> BlockResult {
    match serde_json::to_vec(data) {
        Ok(body) => respond(msg, body, "application/json"),
        Err(e) => error(msg, ErrorCode::Internal, &e.to_string()),
    }
}

/// Return an error [`BlockResult`] with an error code and message.
/// The transport layer maps the error code to a status code.
pub fn error(msg: Message, err_code: ErrorCode, err_message: &str) -> BlockResult {
    BlockResult {
        action: Action::Error,
        error: Some(WaferError {
            code: err_code,
            message: err_message.to_string(),
            meta: Vec::new(),
        }),
        response: None,
        message: Some(msg),
    }
}

/// Return an invalid argument error.
pub fn err_bad_request(msg: Message, message: &str) -> BlockResult {
    error(msg, ErrorCode::InvalidArgument, message)
}

/// Return an unauthenticated error.
pub fn err_unauthorized(msg: Message, message: &str) -> BlockResult {
    error(msg, ErrorCode::Unauthenticated, message)
}

/// Return a permission denied error.
pub fn err_forbidden(msg: Message, message: &str) -> BlockResult {
    error(msg, ErrorCode::PermissionDenied, message)
}

/// Return a not found error.
pub fn err_not_found(msg: Message, message: &str) -> BlockResult {
    error(msg, ErrorCode::NotFound, message)
}

/// Return an already exists error.
pub fn err_conflict(msg: Message, message: &str) -> BlockResult {
    error(msg, ErrorCode::AlreadyExists, message)
}

/// Return a validation error.
pub fn err_validation(msg: Message, message: &str) -> BlockResult {
    error(msg, ErrorCode::InvalidArgument, message)
}

/// Return an internal error.
pub fn err_internal(msg: Message, message: &str) -> BlockResult {
    error(msg, ErrorCode::Internal, message)
}

// ---------------------------------------------------------------------------
// ResponseBuilder
// ---------------------------------------------------------------------------

/// A builder for constructing responses with headers, cookies, and optional status overrides.
///
/// # Example
/// ```ignore
/// let result = ResponseBuilder::new(msg)
///     .set_header("X-Request-Id", "abc123")
///     .set_cookie("session=xyz; HttpOnly; Path=/")
///     .json(&my_data);
/// ```
pub struct ResponseBuilder {
    msg: Message,
    meta: Vec<MetaEntry>,
    cookie_count: usize,
}

impl ResponseBuilder {
    /// Create a new response builder.
    /// Use `.status()` to set an explicit status code for non-200 responses (e.g. redirects).
    pub fn new(msg: Message) -> Self {
        Self {
            msg,
            meta: Vec::new(),
            cookie_count: 0,
        }
    }

    /// Set an explicit status code override (e.g. 201, 301, 302).
    /// If not called, the transport layer uses its default (200 for Respond).
    pub fn status(mut self, status: u16) -> Self {
        self.meta.push(MetaEntry {
            key: META_RESP_STATUS.to_string(),
            value: status.to_string(),
        });
        self
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
            Err(e) => error(self.msg, ErrorCode::Internal, &e.to_string()),
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
pub fn new_response(msg: Message) -> ResponseBuilder {
    ResponseBuilder::new(msg)
}
