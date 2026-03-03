use crate::common::ErrorCode;
use crate::meta::*;
use crate::types::*;
use std::collections::HashMap;

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

/// Encode bytes as lowercase hex string.
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Compute SHA-256 and return as hex string. Used for deterministic hashing (API keys, etc.).
pub fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&sha256(data))
}

/// SHA-256 implementation (FIPS 180-4).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let k: [u32; 64] = [
        0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
        0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
        0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
        0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
        0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
        0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
        0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
        0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4*i], chunk[4*i+1], chunk[4*i+2], chunk[4*i+3]]);
        }
        for i in 16..64 {
            let s0 = w[i-15].rotate_right(7) ^ w[i-15].rotate_right(18) ^ (w[i-15] >> 3);
            let s1 = w[i-2].rotate_right(17) ^ w[i-2].rotate_right(19) ^ (w[i-2] >> 10);
            w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(k[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g; g = f; f = e; e = d.wrapping_add(temp1);
            d = c; c = b; b = a; a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c); h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e); h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g); h[7] = h[7].wrapping_add(hh);
    }

    let mut result = [0u8; 32];
    for (i, &val) in h.iter().enumerate() {
        result[4*i..4*i+4].copy_from_slice(&val.to_be_bytes());
    }
    result
}

/// Convenience function to create a new ResponseBuilder.
pub fn new_response(msg: Message, status: u16) -> ResponseBuilder {
    ResponseBuilder::new(msg, status)
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
                if let Ok(val) = std::env::var(&var_name) {
                    result.push_str(&val);
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
                    if let Ok(val) = std::env::var(&var_name) {
                        result.push_str(&val);
                    }
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}
