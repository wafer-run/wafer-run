// Re-export common helpers from wafer-block.
pub use wafer_block::helpers::{
    err_bad_request, err_conflict, err_forbidden, err_internal, err_not_found, err_unauthorized,
    err_validation, error, json_respond, new_response, respond, respond_empty, respond_json,
    ResponseBuilder,
};

use sha2::Digest;

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
