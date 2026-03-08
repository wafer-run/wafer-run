//! Helper functions and a response builder for common response patterns.

use crate::types::*;

// ---------------------------------------------------------------------------
// Free-standing helper functions
// ---------------------------------------------------------------------------

/// Build a response [`BlockResult`] with a body and content type.
pub fn respond(msg: Message, data: Vec<u8>, content_type: &str) -> BlockResult {
    let mut meta = std::collections::HashMap::new();
    if !content_type.is_empty() {
        meta.insert(META_RESP_CONTENT_TYPE.to_string(), content_type.to_string());
    }
    msg.respond_with(Response { data, meta })
}

/// Serialize `data` as JSON and return a response.
pub fn json_respond<T: serde::Serialize>(msg: Message, data: &T) -> BlockResult {
    match serde_json::to_vec(data) {
        Ok(body) => respond(msg, body, "application/json"),
        Err(e) => error(msg, ERROR_INTERNAL, &e.to_string()),
    }
}

/// Return an error [`BlockResult`] with an error code and message.
pub fn error(msg: Message, err_code: &str, err_message: &str) -> BlockResult {
    BlockResult {
        action: Action::Error,
        error: Some(WaferError::new(err_code, err_message)),
        response: None,
        message: Some(msg),
    }
}

/// Return an invalid argument error.
pub fn err_bad_request(msg: Message, message: &str) -> BlockResult {
    error(msg, ERROR_INVALID_ARGUMENT, message)
}

/// Return an unauthenticated error.
pub fn err_unauthorized(msg: Message, message: &str) -> BlockResult {
    error(msg, ERROR_UNAUTHENTICATED, message)
}

/// Return a permission denied error.
pub fn err_forbidden(msg: Message, message: &str) -> BlockResult {
    error(msg, ERROR_PERMISSION_DENIED, message)
}

/// Return a not found error.
pub fn err_not_found(msg: Message, message: &str) -> BlockResult {
    error(msg, ERROR_NOT_FOUND, message)
}

/// Return an already exists error.
pub fn err_conflict(msg: Message, message: &str) -> BlockResult {
    error(msg, ERROR_ALREADY_EXISTS, message)
}

/// Return a validation error.
pub fn err_validation(msg: Message, message: &str) -> BlockResult {
    error(msg, ERROR_INVALID_ARGUMENT, message)
}

/// Return an internal error.
pub fn err_internal(msg: Message, message: &str) -> BlockResult {
    error(msg, ERROR_INTERNAL, message)
}

// ---------------------------------------------------------------------------
// ResponseBuilder
// ---------------------------------------------------------------------------

/// A builder for constructing responses with headers, cookies, and optional status overrides.
pub struct ResponseBuilder {
    msg: Message,
    meta: std::collections::HashMap<String, String>,
}

impl ResponseBuilder {
    pub fn new(msg: Message) -> Self {
        Self {
            msg,
            meta: std::collections::HashMap::new(),
        }
    }

    pub fn status(mut self, status: u16) -> Self {
        self.meta.insert(META_RESP_STATUS.to_string(), status.to_string());
        self
    }

    pub fn set_cookie(mut self, cookie: &str) -> Self {
        let idx = self.meta.keys().filter(|k| k.starts_with(META_RESP_COOKIE_PREFIX)).count();
        self.meta.insert(format!("{}{}", META_RESP_COOKIE_PREFIX, idx), cookie.to_string());
        self
    }

    pub fn set_header(mut self, key: &str, value: &str) -> Self {
        self.meta.insert(format!("{}{}", META_RESP_HEADER_PREFIX, key), value.to_string());
        self
    }

    pub fn json<T: serde::Serialize>(mut self, data: &T) -> BlockResult {
        match serde_json::to_vec(data) {
            Ok(body) => {
                self.meta.insert(META_RESP_CONTENT_TYPE.to_string(), "application/json".to_string());
                self.msg.respond_with(Response {
                    data: body,
                    meta: self.meta,
                })
            }
            Err(e) => error(self.msg, ERROR_INTERNAL, &e.to_string()),
        }
    }

    pub fn body(mut self, data: Vec<u8>, content_type: &str) -> BlockResult {
        if !content_type.is_empty() {
            self.meta.insert(META_RESP_CONTENT_TYPE.to_string(), content_type.to_string());
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
