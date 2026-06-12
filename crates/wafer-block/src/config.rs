//! Block configuration — common config accessor for blocks.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::LifecycleEvent;

/// Parsed block configuration from lifecycle event data.
pub struct BlockConfig {
    inner: Option<serde_json::Value>,
}

impl BlockConfig {
    /// Parse the JSON payload from a [`LifecycleEvent`] (e.g. the `Start`
    /// event carrying per-flow-step config) into a [`BlockConfig`]. Invalid
    /// or empty payloads yield an empty config (all lookups return defaults).
    pub fn from_event(event: &LifecycleEvent) -> Self {
        let inner = if !event.data.is_empty() {
            serde_json::from_slice(&event.data).ok()
        } else {
            None
        };
        Self { inner }
    }

    /// Return the string-typed value for `key`, or `""` if missing or
    /// the value is not a JSON string.
    pub fn str(&self, key: &str) -> &str {
        self.inner
            .as_ref()
            .and_then(|c| c.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }

    /// Return the string-typed value for `key`, or `default` when the key
    /// is missing, not a JSON string, or empty.
    pub fn str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        let v = self.str(key);
        if v.is_empty() {
            default
        } else {
            v
        }
    }

    /// Return the boolean value for `key`. Accepts a JSON boolean or a
    /// string `"true"` / `"false"` — block config arrives in both shapes
    /// (typed JSON from `add_block_config`, stringified values from
    /// flow-step config; `parse_config_map` already treats them as
    /// interchangeable). Returns `None` when the key is missing or the
    /// value is neither shape.
    pub fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            serde_json::Value::Bool(b) => Some(*b),
            serde_json::Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Return the string entries of the JSON array at `key`, or `None`
    /// when the key is missing or not an array. Non-string entries are
    /// dropped.
    pub fn str_array(&self, key: &str) -> Option<Vec<String>> {
        let items = self.get(key)?.as_array()?;
        Some(
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        )
    }

    /// Return the raw JSON value for `key`, or `None` if absent.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.inner.as_ref().and_then(|c| c.get(key))
    }

    /// Borrow the inner JSON value. Returns a static reference to
    /// `serde_json::Value::Null` when this config has no payload —
    /// callers that only need to read fields can treat the result as a
    /// JSON object root.
    pub fn as_value(&self) -> &serde_json::Value {
        static NULL: serde_json::Value = serde_json::Value::Null;
        self.inner.as_ref().unwrap_or(&NULL)
    }

    /// Parse the standard `block` / `flow` keys into a [`DispatchTarget`].
    pub fn dispatch_target(&self) -> Option<DispatchTarget> {
        DispatchTarget::from_config(self.inner.as_ref())
    }
}

/// Flatten the top-level keys of a JSON object into a `HashMap<String,
/// String>`. Strings, numbers, and booleans are stringified; nested
/// objects/arrays are ignored.
pub fn parse_config_map(config: &serde_json::Value) -> std::collections::HashMap<String, String> {
    let mut cfg = std::collections::HashMap::new();
    if let Some(obj) = config.as_object() {
        for (k, v) in obj {
            match v {
                serde_json::Value::String(s) => {
                    cfg.insert(k.clone(), s.clone());
                }
                serde_json::Value::Number(n) => {
                    cfg.insert(k.clone(), n.to_string());
                }
                serde_json::Value::Bool(b) => {
                    cfg.insert(k.clone(), b.to_string());
                }
                _ => {}
            }
        }
    }
    cfg
}

/// Parse a duration string of the form `"30s"`, `"500ms"`, `"5m"`, `"2h"`
/// (or a bare integer = seconds). Returns [`Duration::ZERO`] on empty or
/// malformed input (logged as a warning).
pub fn parse_duration(s: &str) -> Duration {
    if s.is_empty() {
        return Duration::ZERO;
    }
    let s = s.trim();
    let result = if let Some(rest) = s.strip_suffix("ms") {
        rest.parse::<u64>().map(Duration::from_millis)
    } else if let Some(rest) = s.strip_suffix('s') {
        rest.parse::<u64>().map(Duration::from_secs)
    } else if let Some(rest) = s.strip_suffix('m') {
        rest.parse::<u64>().map(|m| Duration::from_secs(m * 60))
    } else if let Some(rest) = s.strip_suffix('h') {
        rest.parse::<u64>().map(|h| Duration::from_secs(h * 3600))
    } else {
        s.parse::<u64>().map(Duration::from_secs)
    };
    match result {
        Ok(d) => d,
        Err(_) => {
            tracing::warn!(input = %s, "invalid duration string, defaulting to zero");
            Duration::ZERO
        }
    }
}

/// A dispatch target: either a flow or a single block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DispatchTarget {
    /// Dispatch to a named flow.
    Flow(String),
    /// Dispatch directly to a single block by name.
    Block(String),
}

impl DispatchTarget {
    /// Extract a [`DispatchTarget`] from the standard `block` / `flow` keys
    /// of a JSON config object. `block` takes precedence over `flow`.
    pub fn from_config(config: Option<&serde_json::Value>) -> Option<Self> {
        let block = config
            .and_then(|c| c.get("block"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !block.is_empty() {
            return Some(DispatchTarget::Block(block.to_string()));
        }
        let flow = config
            .and_then(|c| c.get("flow"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !flow.is_empty() {
            return Some(DispatchTarget::Flow(flow.to_string()));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LifecycleType;

    fn config_from(json: serde_json::Value) -> BlockConfig {
        BlockConfig::from_event(&LifecycleEvent {
            event_type: LifecycleType::Init,
            data: serde_json::to_vec(&json).expect("serialize test config"),
        })
    }

    fn empty_config() -> BlockConfig {
        BlockConfig::from_event(&LifecycleEvent {
            event_type: LifecycleType::Init,
            data: Vec::new(),
        })
    }

    #[test]
    fn str_or_falls_back_on_missing_empty_or_non_string() {
        let cfg = config_from(serde_json::json!({
            "set": "value",
            "empty": "",
            "number": 7,
        }));
        assert_eq!(cfg.str_or("set", "default"), "value");
        assert_eq!(cfg.str_or("empty", "default"), "default");
        assert_eq!(cfg.str_or("number", "default"), "default");
        assert_eq!(cfg.str_or("missing", "default"), "default");
        assert_eq!(empty_config().str_or("any", "default"), "default");
    }

    #[test]
    fn bool_accepts_json_bool_and_string_forms() {
        let cfg = config_from(serde_json::json!({
            "typed_true": true,
            "typed_false": false,
            "string_true": "true",
            "string_false": "false",
            "garbage": "yes",
            "number": 1,
        }));
        assert_eq!(cfg.bool("typed_true"), Some(true));
        assert_eq!(cfg.bool("typed_false"), Some(false));
        assert_eq!(cfg.bool("string_true"), Some(true));
        assert_eq!(cfg.bool("string_false"), Some(false));
        assert_eq!(cfg.bool("garbage"), None);
        assert_eq!(cfg.bool("number"), None);
        assert_eq!(cfg.bool("missing"), None);
        assert_eq!(empty_config().bool("any"), None);
    }

    #[test]
    fn str_array_keeps_strings_and_drops_other_entries() {
        let cfg = config_from(serde_json::json!({
            "roles": ["admin", "developer"],
            "mixed": [42, "kept", true],
            "empty": [],
            "not_array": "admin",
        }));
        assert_eq!(
            cfg.str_array("roles"),
            Some(vec!["admin".to_string(), "developer".to_string()]),
        );
        assert_eq!(cfg.str_array("mixed"), Some(vec!["kept".to_string()]));
        assert_eq!(cfg.str_array("empty"), Some(Vec::new()));
        assert_eq!(cfg.str_array("not_array"), None);
        assert_eq!(cfg.str_array("missing"), None);
        assert_eq!(empty_config().str_array("any"), None);
    }
}
