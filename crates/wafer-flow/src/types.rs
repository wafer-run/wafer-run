use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A complete WaferFlow definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaferFlow {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<PortSchema>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<PortSchema>,
    pub steps: Vec<Step>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<FlowConfig>,
    /// Block dependencies — the runtime ensures these exist before resolving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<String>>,
    /// Declarative config routing: maps user-facing config keys to target block configs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_map: Option<HashMap<String, ConfigMapEntry>>,
    /// Static config values always injected into target block configs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_defaults: Option<HashMap<String, serde_json::Value>>,
}

/// A single step in a flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub block: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Vec<NextEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub each: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel: Option<Vec<ParallelBranch>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Per-step block config (passed to RuntimeContext.config).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

/// A branch in a parallel step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelBranch {
    pub steps: Vec<Step>,
}

/// Conditional routing entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
}

/// JSON Schema subset for typed ports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortSchema {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<std::collections::HashMap<String, PortSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<PortSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

/// Flow-level configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_error: Option<String>,
}

/// A declarative config routing entry: maps a user-facing config key
/// to a target block and key within that block's config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigMapEntry {
    pub target: String,
    pub key: String,
}

/// Lightweight flow info for introspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Block definition metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockDef {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<PortSchema>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<PortSchema>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}
