//! Block configuration — common config accessor for blocks.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::LifecycleEvent;

/// Parsed block configuration from lifecycle event data.
pub struct BlockConfig {
    inner: Option<serde_json::Value>,
}

impl BlockConfig {
    pub fn from_event(event: &LifecycleEvent) -> Self {
        let inner = if !event.data.is_empty() {
            serde_json::from_slice(&event.data).ok()
        } else {
            None
        };
        Self { inner }
    }

    pub fn str(&self, key: &str) -> &str {
        self.inner
            .as_ref()
            .and_then(|c| c.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }

    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.inner.as_ref().and_then(|c| c.get(key))
    }

    pub fn dispatch_target(&self) -> Option<DispatchTarget> {
        DispatchTarget::from_config(self.inner.as_ref())
    }
}

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
    Flow(String),
    Block(String),
}

impl DispatchTarget {
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
