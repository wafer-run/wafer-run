use crate::common::ErrorCode;
use crate::context::Context;
use crate::meta::*;
use crate::types::*;
use std::collections::HashMap;

/// Log sends a log message through the context.
pub fn log(ctx: &dyn Context, level: &str, message: &str) {
    let msg = Message {
        kind: "log".to_string(),
        meta: {
            let mut m = HashMap::new();
            m.insert("level".to_string(), level.to_string());
            m
        },
        data: message.as_bytes().to_vec(),
    };
    ctx.send(&msg);
}

/// ConfigGet retrieves a configuration value through the context.
pub fn config_get(ctx: &dyn Context, key: &str) -> Option<String> {
    let msg = Message {
        kind: "config.get".to_string(),
        meta: {
            let mut m = HashMap::new();
            m.insert("key".to_string(), key.to_string());
            m
        },
        data: Vec::new(),
    };
    let result = ctx.send(&msg);
    if result.action == Action::Error || result.response.is_none() {
        return None;
    }
    result
        .response
        .map(|r| String::from_utf8_lossy(&r.data).to_string())
}

/// Respond returns a Result with a response body and status code.
pub fn respond(msg: Message, status: u16, data: Vec<u8>, content_type: &str) -> Result_ {
    let mut meta = HashMap::new();
    meta.insert(META_RESP_STATUS.to_string(), status.to_string());
    if !content_type.is_empty() {
        meta.insert(META_RESP_CONTENT_TYPE.to_string(), content_type.to_string());
    }
    msg.respond(Response { data, meta })
}

/// Error returns an error Result with a status code.
pub fn error(msg: Message, status: u16, err_code: &str, err_message: &str) -> Result_ {
    Result_ {
        action: Action::Error,
        error: Some(
            WaferError::new(err_code, err_message)
                .with_meta(META_RESP_STATUS, status.to_string()),
        ),
        response: None,
        message: Some(msg),
    }
}

/// JSONRespond marshals data as JSON and returns a response.
pub fn json_respond<T: serde::Serialize>(msg: Message, status: u16, data: &T) -> Result_ {
    match serde_json::to_vec(data) {
        Ok(body) => respond(msg, status, body, "application/json"),
        Err(e) => error(msg, 500, ErrorCode::INTERNAL, &e.to_string()),
    }
}

/// ErrBadRequest returns a 400 error.
pub fn err_bad_request(msg: Message, message: &str) -> Result_ {
    error(msg, 400, ErrorCode::INVALID_ARGUMENT, message)
}

/// ErrUnauthorized returns a 401 error.
pub fn err_unauthorized(msg: Message, message: &str) -> Result_ {
    error(msg, 401, ErrorCode::UNAUTHENTICATED, message)
}

/// ErrForbidden returns a 403 error.
pub fn err_forbidden(msg: Message, message: &str) -> Result_ {
    error(msg, 403, ErrorCode::PERMISSION_DENIED, message)
}

/// ErrNotFound returns a 404 error.
pub fn err_not_found(msg: Message, message: &str) -> Result_ {
    error(msg, 404, ErrorCode::NOT_FOUND, message)
}

/// ErrConflict returns a 409 error.
pub fn err_conflict(msg: Message, message: &str) -> Result_ {
    error(msg, 409, ErrorCode::ALREADY_EXISTS, message)
}

/// ErrValidation returns a 422 error.
pub fn err_validation(msg: Message, message: &str) -> Result_ {
    error(msg, 422, ErrorCode::INVALID_ARGUMENT, message)
}

/// ErrInternal returns a 500 error.
pub fn err_internal(msg: Message, message: &str) -> Result_ {
    error(msg, 500, ErrorCode::INTERNAL, message)
}

/// ResponseBuilder builds responses with headers and cookies.
pub struct ResponseBuilder {
    msg: Message,
    status: u16,
    meta: HashMap<String, String>,
    cookie_count: usize,
}

impl ResponseBuilder {
    /// Create a new response builder.
    pub fn new(msg: Message, status: u16) -> Self {
        let mut meta = HashMap::new();
        meta.insert(META_RESP_STATUS.to_string(), status.to_string());
        Self {
            msg,
            status,
            meta,
            cookie_count: 0,
        }
    }

    /// SetCookie adds a Set-Cookie header to the response.
    pub fn set_cookie(mut self, cookie: &str) -> Self {
        self.meta.insert(
            format!("{}{}", META_RESP_COOKIE_PREFIX, self.cookie_count),
            cookie.to_string(),
        );
        self.cookie_count += 1;
        self
    }

    /// SetHeader adds a response header.
    pub fn set_header(mut self, key: &str, value: &str) -> Self {
        self.meta.insert(
            format!("{}{}", META_RESP_HEADER_PREFIX, key),
            value.to_string(),
        );
        self
    }

    /// JSON marshals data as JSON and returns a response.
    pub fn json<T: serde::Serialize>(mut self, data: &T) -> Result_ {
        match serde_json::to_vec(data) {
            Ok(body) => {
                self.meta.insert(
                    META_RESP_CONTENT_TYPE.to_string(),
                    "application/json".to_string(),
                );
                self.msg.respond(Response {
                    data: body,
                    meta: self.meta,
                })
            }
            Err(e) => error(self.msg, 500, ErrorCode::INTERNAL, &e.to_string()),
        }
    }

    /// Body sets the response body with the given content type.
    pub fn body(mut self, data: Vec<u8>, content_type: &str) -> Result_ {
        if !content_type.is_empty() {
            self.meta.insert(
                META_RESP_CONTENT_TYPE.to_string(),
                content_type.to_string(),
            );
        }
        self.msg.respond(Response {
            data,
            meta: self.meta,
        })
    }
}

/// Convenience function to create a new ResponseBuilder.
pub fn new_response(msg: Message, status: u16) -> ResponseBuilder {
    ResponseBuilder::new(msg, status)
}
