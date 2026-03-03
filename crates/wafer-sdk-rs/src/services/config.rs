//! Config service client — calls `wafer/config` block via `call-block`.

use serde::{Deserialize, Serialize};

use crate::wafer::block_world::runtime;
use crate::wafer::block_world::types::{Action, Message};

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

// --- Helpers ---

fn make_msg(kind: &str, data: &impl Serialize) -> Message {
    Message {
        kind: kind.to_string(),
        data: serde_json::to_vec(data).unwrap_or_default(),
        meta: Vec::new(),
    }
}

// --- Public API ---

/// Retrieve a configuration value by key, returning `None` if not found.
pub fn get(key: &str) -> Option<String> {
    let msg = make_msg("config.get", &GetReq { key });
    let result = runtime::call_block("wafer/config", &msg);
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
    let msg = make_msg("config.set", &SetReq { key, value });
    let _ = runtime::call_block("wafer/config", &msg);
}
