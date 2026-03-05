use crate::common::ErrorCode;
use crate::meta::*;
use crate::types::*;
use std::collections::HashMap;

/// Respond returns a Result with a response body and content type.
/// The transport layer determines the status code (default 200 for Respond).
pub fn respond(msg: Message, data: Vec<u8>, content_type: &str) -> Result_ {
    let mut meta = HashMap::new();
    if !content_type.is_empty() {
        meta.insert(META_RESP_CONTENT_TYPE.to_string(), content_type.to_string());
    }
    msg.respond(Response { data, meta })
}

/// Error returns an error Result with an error code and message.
/// The transport layer maps the error code to a status code.
pub fn error(msg: Message, err_code: &str, err_message: &str) -> Result_ {
    Result_ {
        action: Action::Error,
        error: Some(WaferError::new(err_code, err_message)),
        response: None,
        message: Some(msg),
    }
}

/// JSONRespond marshals data as JSON and returns a response.
pub fn json_respond<T: serde::Serialize>(msg: Message, data: &T) -> Result_ {
    match serde_json::to_vec(data) {
        Ok(body) => respond(msg, body, "application/json"),
        Err(e) => error(msg, ErrorCode::INTERNAL, &e.to_string()),
    }
}

/// ErrBadRequest returns a 400 error.
pub fn err_bad_request(msg: Message, message: &str) -> Result_ {
    error(msg, ErrorCode::INVALID_ARGUMENT, message)
}

/// ErrUnauthorized returns a 401 error.
pub fn err_unauthorized(msg: Message, message: &str) -> Result_ {
    error(msg, ErrorCode::UNAUTHENTICATED, message)
}

/// ErrForbidden returns a 403 error.
pub fn err_forbidden(msg: Message, message: &str) -> Result_ {
    error(msg, ErrorCode::PERMISSION_DENIED, message)
}

/// ErrNotFound returns a 404 error.
pub fn err_not_found(msg: Message, message: &str) -> Result_ {
    error(msg, ErrorCode::NOT_FOUND, message)
}

/// ErrConflict returns a 409 error.
pub fn err_conflict(msg: Message, message: &str) -> Result_ {
    error(msg, ErrorCode::ALREADY_EXISTS, message)
}

/// ErrValidation returns a 422 error.
pub fn err_validation(msg: Message, message: &str) -> Result_ {
    error(msg, ErrorCode::INVALID_ARGUMENT, message)
}

/// ErrInternal returns a 500 error.
pub fn err_internal(msg: Message, message: &str) -> Result_ {
    error(msg, ErrorCode::INTERNAL, message)
}

/// ResponseBuilder builds responses with headers and cookies.
pub struct ResponseBuilder {
    msg: Message,
    meta: HashMap<String, String>,
    cookie_count: usize,
}

impl ResponseBuilder {
    /// Create a new response builder.
    /// Use `.status()` to set an explicit status code for non-200 responses (e.g. redirects).
    pub fn new(msg: Message) -> Self {
        Self {
            msg,
            meta: HashMap::new(),
            cookie_count: 0,
        }
    }

    /// Set an explicit status code override (e.g. 201, 301, 302).
    /// If not called, the transport layer uses its default (200 for Respond).
    pub fn status(mut self, status: u16) -> Self {
        self.meta
            .insert(META_RESP_STATUS.to_string(), status.to_string());
        self
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
            Err(e) => error(self.msg, ErrorCode::INTERNAL, &e.to_string()),
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

/// Respond with serialised JSON (no HTTP meta). Used by internal service blocks.
pub fn respond_json<T: serde::Serialize>(msg: &Message, data: &T) -> Result_ {
    match serde_json::to_vec(data) {
        Ok(body) => msg.clone().respond(Response {
            data: body,
            meta: HashMap::new(),
        }),
        Err(e) => Result_::error(WaferError::new(ErrorCode::INTERNAL, e.to_string())),
    }
}

/// Respond with an empty body (no data, no meta). Used by service blocks for void operations.
pub fn respond_empty(msg: &Message) -> Result_ {
    Result_ {
        action: Action::Respond,
        response: Some(Response {
            data: Vec::new(),
            meta: HashMap::new(),
        }),
        error: None,
        message: Some(msg.clone()),
    }
}

/// Convenience function to create a new ResponseBuilder.
pub fn new_response(msg: Message) -> ResponseBuilder {
    ResponseBuilder::new(msg)
}

/// Encode bytes as lowercase hex string.
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        result.push(HEX[(b >> 4) as usize] as char);
        result.push(HEX[(b & 0xf) as usize] as char);
    }
    result
}

/// Compute SHA-256 and return as hex string. Used for deterministic hashing (API keys, etc.).
pub fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&sha256(data))
}

/// SHA-256 hash.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Expand environment variables in the format $VAR or ${VAR}.
pub fn expand_env_vars(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            if chars.peek() == Some(&'{') {
                chars.next(); // consume '{'
                let mut var_name = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc == '}' {
                        chars.next();
                        break;
                    }
                    var_name.push(nc);
                    chars.next();
                }
                match std::env::var(&var_name) {
                    Ok(val) => result.push_str(&val),
                    Err(_) => tracing::warn!(var = %var_name, "undefined environment variable referenced"),
                }
            } else {
                let mut var_name = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc.is_alphanumeric() || nc == '_' {
                        var_name.push(nc);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if !var_name.is_empty() {
                    match std::env::var(&var_name) {
                        Ok(val) => result.push_str(&val),
                        Err(_) => tracing::warn!(var = %var_name, "undefined environment variable referenced"),
                    }
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}
