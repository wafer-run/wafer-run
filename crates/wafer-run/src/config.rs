use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::types::{InstanceMode, LifecycleEvent};

// ---------------------------------------------------------------------------
// BlockConfig — common config accessor for blocks
// ---------------------------------------------------------------------------

/// Parsed block configuration from lifecycle event data.
///
/// Provides typed accessors so blocks don't need to repeat the
/// `event.data → serde_json → get(key) → as_str` boilerplate.
pub struct BlockConfig {
    inner: Option<serde_json::Value>,
}

impl BlockConfig {
    /// Parse config from lifecycle event data.
    pub fn from_event(event: &LifecycleEvent) -> Self {
        let inner = if !event.data.is_empty() {
            serde_json::from_slice(&event.data).ok()
        } else {
            None
        };
        Self { inner }
    }

    /// Get a string config value (empty string if missing).
    pub fn str(&self, key: &str) -> &str {
        self.inner
            .as_ref()
            .and_then(|c| c.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }

    /// Get a raw JSON value by key.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.inner.as_ref().and_then(|c| c.get(key))
    }

    /// Get a string value with env var override taking precedence.
    pub fn env_or(&self, env_var: &str, key: &str) -> Option<String> {
        if let Ok(val) = std::env::var(env_var) {
            if !val.is_empty() {
                return Some(val);
            }
        }
        self.inner
            .as_ref()
            .and_then(|c| c.get(key))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Parse a dispatch target (`"block"` or `"flow"`) from config.
    pub fn dispatch_target(&self) -> Option<DispatchTarget> {
        DispatchTarget::from_config(self.inner.as_ref())
    }
}

// ---------------------------------------------------------------------------
// Flow & Node definitions
// ---------------------------------------------------------------------------

/// FlowDef defines a flow in JSON configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowDef {
    pub id: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub config: FlowConfigDef,
    pub root: NodeDef,
}

/// FlowConfigDef holds flow-level configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlowConfigDef {
    #[serde(default)]
    pub on_error: String,
    #[serde(default)]
    pub timeout: String,
}

/// NodeDef defines a node in the flow tree (JSON serialization).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDef {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub block: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub flow: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub r#match: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next: Vec<NodeDef>,
}

/// Flow is the runtime representation of a flow.
pub struct Flow {
    pub id: String,
    pub summary: String,
    pub config: FlowConfig,
    pub root: Box<Node>,
}

/// FlowConfig holds runtime flow configuration.
#[derive(Debug, Clone)]
pub struct FlowConfig {
    pub on_error: String,
    pub timeout: Duration,
}

impl Default for FlowConfig {
    fn default() -> Self {
        Self {
            on_error: "stop".to_string(),
            timeout: Duration::ZERO,
        }
    }
}

/// Node is the runtime representation of a flow node.
pub struct Node {
    pub block: String,
    pub flow: String,
    pub match_pattern: String,
    pub config: Option<serde_json::Value>,
    pub instance: Option<InstanceMode>,
    pub next: Vec<Box<Node>>,

    // Resolved at startup
    pub(crate) resolved_block: Option<std::sync::Arc<dyn crate::block::Block>>,
    pub(crate) config_map: std::collections::HashMap<String, String>,
}

impl Node {
    pub fn new() -> Self {
        Self {
            block: String::new(),
            flow: String::new(),
            match_pattern: String::new(),
            config: None,
            instance: None,
            next: Vec::new(),
            resolved_block: None,
            config_map: std::collections::HashMap::new(),
        }
    }
}

/// Parse config JSON into a map of string key-value pairs.
/// String values are used as-is. Numbers and booleans are converted to their
/// string representation. Objects and arrays are skipped.
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
                _ => {} // skip null, objects, arrays
            }
        }
    }
    cfg
}

/// Parse a duration string like "30s", "5m", "1h".
/// Returns `Duration::ZERO` and logs a warning for invalid input.
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

/// Convert a FlowDef to a runtime Flow.
pub fn flow_def_to_flow(def: &FlowDef) -> Flow {
    Flow {
        id: def.id.clone(),
        summary: def.summary.clone(),
        config: FlowConfig {
            on_error: if def.config.on_error.is_empty() {
                "stop".to_string()
            } else {
                def.config.on_error.clone()
            },
            timeout: parse_duration(&def.config.timeout),
        },
        root: Box::new(node_def_to_node(&def.root)),
    }
}

/// Convert a NodeDef to a runtime Node.
fn node_def_to_node(def: &NodeDef) -> Node {
    Node {
        block: def.block.clone(),
        flow: def.flow.clone(),
        match_pattern: def.r#match.clone(),
        config: def.config.clone(),
        instance: if def.instance.is_empty() {
            None
        } else {
            Some(InstanceMode::parse(&def.instance))
        },
        next: def.next.iter().map(|n| Box::new(node_def_to_node(n))).collect(),
        resolved_block: None,
        config_map: std::collections::HashMap::new(),
    }
}

/// Convert a runtime Flow back to a FlowDef.
pub fn flow_to_flow_def(c: &Flow) -> FlowDef {
    FlowDef {
        id: c.id.clone(),
        summary: c.summary.clone(),
        config: FlowConfigDef {
            on_error: c.config.on_error.clone(),
            timeout: if c.config.timeout.is_zero() {
                String::new()
            } else {
                format!("{}s", c.config.timeout.as_secs())
            },
        },
        root: node_to_node_def(&c.root),
    }
}

/// Convert a runtime Node to a NodeDef.
fn node_to_node_def(n: &Node) -> NodeDef {
    NodeDef {
        block: n.block.clone(),
        flow: n.flow.clone(),
        r#match: n.match_pattern.clone(),
        config: n.config.clone(),
        instance: n.instance.map(|m| m.to_string()).unwrap_or_default(),
        next: n.next.iter().map(|child| node_to_node_def(child)).collect(),
    }
}

/// A dispatch target: either a flow or a single block.
/// This is the same `block` XOR `flow` pattern used in `NodeDef`.
#[derive(Debug, Clone)]
pub enum DispatchTarget {
    Flow(String),
    Block(String),
}

impl DispatchTarget {
    /// Parse a dispatch target from a JSON config object.
    /// Checks `"block"` first; falls back to `"flow"`.
    /// Returns `None` if neither is present or both are empty.
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

/// FlowInfo provides read-only info about a flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowInfo {
    pub id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub on_error: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub timeout: String,
}
