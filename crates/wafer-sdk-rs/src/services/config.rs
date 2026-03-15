//! Config service client — calls `wafer/config` block via `call_block`.

use serde::{Deserialize, Serialize};

use crate::types::{Action, Message};
use crate::call_block;

// --- Internal request/response types ---

#[derive(Serialize)]
struct GetReq<'a> {
    key: &'a str,
}

#[derive(Serialize)]
struct SetReq<'a> {
    key: &'a str,
    value: &'a str,
}

#[derive(Deserialize)]
struct GetResp {
    value: Option<String>,
}

// --- Public API ---

/// Retrieve a configuration value by key, returning `None` if not found.
pub fn get(key: &str) -> Option<String> {
    let msg = Message::new("config.get", serde_json::to_vec(&GetReq { key }).unwrap_or_default());
    let result = call_block("wafer-run/config", &msg);
    match result.action {
        Action::Error => None,
        _ => {
            let data = result.response.map(|r| r.data).unwrap_or_default();
            if data.is_empty() {
                return None;
            }
            let resp: GetResp = serde_json::from_slice(&data).ok()?;
            resp.value
        }
    }
}

/// Retrieve a config value, returning `default_value` if the key is absent.
pub fn get_default(key: &str, default_value: &str) -> String {
    get(key).unwrap_or_else(|| default_value.to_string())
}

/// Store a configuration key-value pair.
pub fn set(key: &str, value: &str) {
    let msg = Message::new("config.set", serde_json::to_vec(&SetReq { key, value }).unwrap_or_default());
    let _ = call_block("wafer-run/config", &msg);
}
