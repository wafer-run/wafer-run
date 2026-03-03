use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::types::InstanceMode;

/// ChainDef defines a chain in JSON configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainDef {
    pub id: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub config: ChainConfigDef,
    pub root: NodeDef,
}

/// ChainConfigDef holds chain-level configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChainConfigDef {
    #[serde(default)]
    pub on_error: String,
    #[serde(default)]
    pub timeout: String,
}

/// NodeDef defines a node in the chain tree (JSON serialization).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDef {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub block: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub chain: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub r#match: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next: Vec<NodeDef>,
}

/// Chain is the runtime representation of a chain.
pub struct Chain {
    pub id: String,
    pub summary: String,
    pub config: ChainConfig,
    pub root: Box<Node>,
}

/// ChainConfig holds runtime chain configuration.
#[derive(Debug, Clone)]
pub struct ChainConfig {
    pub on_error: String,
    pub timeout: Duration,
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self {
            on_error: "stop".to_string(),
            timeout: Duration::ZERO,
        }
    }
}

/// Node is the runtime representation of a chain node.
pub struct Node {
    pub block: String,
    pub chain: String,
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
            chain: String::new(),
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
pub fn parse_config_map(config: &serde_json::Value) -> std::collections::HashMap<String, String> {
    let mut cfg = std::collections::HashMap::new();
    if let Some(obj) = config.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                cfg.insert(k.clone(), s.to_string());
            }
        }
    }
    cfg
}

/// Parse a duration string like "30s", "5m", "1h".
pub fn parse_duration(s: &str) -> Duration {
    if s.is_empty() {
        return Duration::ZERO;
    }
    let s = s.trim();
    if let Some(rest) = s.strip_suffix("ms") {
        rest.parse::<u64>()
            .map(Duration::from_millis)
            .unwrap_or(Duration::ZERO)
    } else if let Some(rest) = s.strip_suffix('s') {
        rest.parse::<u64>()
            .map(Duration::from_secs)
            .unwrap_or(Duration::ZERO)
    } else if let Some(rest) = s.strip_suffix('m') {
        rest.parse::<u64>()
            .map(|m| Duration::from_secs(m * 60))
            .unwrap_or(Duration::ZERO)
    } else if let Some(rest) = s.strip_suffix('h') {
        rest.parse::<u64>()
            .map(|h| Duration::from_secs(h * 3600))
            .unwrap_or(Duration::ZERO)
    } else {
        s.parse::<u64>()
            .map(Duration::from_secs)
            .unwrap_or(Duration::ZERO)
    }
}

/// Convert a ChainDef to a runtime Chain.
pub fn chain_def_to_chain(def: &ChainDef) -> Chain {
    Chain {
        id: def.id.clone(),
        summary: def.summary.clone(),
        config: ChainConfig {
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
        chain: def.chain.clone(),
        match_pattern: def.r#match.clone(),
        config: def.config.clone(),
        instance: if def.instance.is_empty() {
            None
        } else {
            InstanceMode::parse(&def.instance)
        },
        next: def.next.iter().map(|n| Box::new(node_def_to_node(n))).collect(),
        resolved_block: None,
        config_map: std::collections::HashMap::new(),
    }
}

/// Convert a runtime Chain back to a ChainDef.
pub fn chain_to_chain_def(c: &Chain) -> ChainDef {
    ChainDef {
        id: c.id.clone(),
        summary: c.summary.clone(),
        config: ChainConfigDef {
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
        chain: n.chain.clone(),
        r#match: n.match_pattern.clone(),
        config: n.config.clone(),
        instance: n.instance.map(|m| m.to_string()).unwrap_or_default(),
        next: n.next.iter().map(|child| node_to_node_def(child)).collect(),
    }
}

/// ChainInfo provides read-only info about a chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainInfo {
    pub id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub on_error: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub timeout: String,
}
