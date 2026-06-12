//! Data model for a parsed WaferFlow document.
//!
//! These structs mirror the JSON schema published at
//! `site/public/schema/waferflow/` and are produced by [`crate::parse`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A complete WaferFlow definition: metadata, typed input/output ports,
/// the ordered list of [`Step`]s to execute, and optional flow-level config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaferFlow {
    /// Stable identifier for the flow (used by the runtime registry).
    pub id: String,
    /// Human-readable name displayed in admin UIs and logs.
    pub name: String,
    /// SemVer string for the flow definition itself (not the runtime).
    pub version: String,
    /// Free-form description shown in introspection endpoints.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema describing the data the caller must supply as `$.input`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<PortSchema>,
    /// JSON Schema describing the value returned to the caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<PortSchema>,
    /// Ordered list of steps; execution starts at `steps[0]` unless a
    /// `next` entry redirects.
    pub steps: Vec<Step>,
    /// Optional flow-level execution policy (timeouts, step budget, error handling).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<FlowConfig>,
    /// Block dependencies — the runtime ensures these blocks are registered
    /// before the flow is allowed to execute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<String>>,
    /// Declarative config routing: maps user-facing config keys to
    /// `(target block, key within that block's config)` pairs so a flow
    /// can expose a flat config surface that fans out to its component blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_map: Option<HashMap<String, ConfigMapEntry>>,
    /// Static config values always injected into target block configs at
    /// resolve time (in addition to anything routed via `config_map`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_defaults: Option<HashMap<String, serde_json::Value>>,
}

/// A single step in a flow — one block invocation plus its input template
/// and (optionally) the routing rules for the next step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Step identifier unique within the flow; also the key under which
    /// the step's output is stored in the [`crate::Accumulator`].
    pub id: String,
    /// Registered block name to invoke (e.g. `"crypto/jwt-sign"`).
    pub block: String,
    /// JSON template passed to the block as input. String values starting
    /// with `$.` are resolved against the accumulator at execution time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    /// Conditional routing entries evaluated in order; the first whose
    /// `when` is true (or the lone default entry) wins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Vec<NextEntry>>,
    /// `$.path` to an array whose elements drive a per-item fan-out.
    /// The inner step iteration sees the current item as `$.each.item`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub each: Option<String>,
    /// Parallel branches run concurrently; the step completes when all
    /// branches finish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel: Option<Vec<ParallelBranch>>,
    /// Human-readable description shown in introspection endpoints.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Per-step block config merged into `RuntimeContext.config` for this
    /// invocation only (overrides flow- and instance-level config).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

/// One branch of a [`Step::parallel`] fan-out. Each branch is its own
/// ordered list of steps that executes concurrently with its siblings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelBranch {
    /// Ordered steps run within this branch.
    pub steps: Vec<Step>,
}

/// A single routing rule inside [`Step::next`]: when `when` evaluates to
/// true, jump to `step` (within this flow) or `flow` (a different flow id).
/// An entry without `when` is the default fallthrough.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextEntry {
    /// Optional `when` expression. Omit for the default fallthrough entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Target step id within the current flow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Target flow id to hand control over to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
}

/// JSON Schema subset used to describe typed flow / step ports.
///
/// Mirrors a handful of JSON Schema keywords (`type`, `properties`, `items`,
/// `required`, `default`, `description`) — enough for the introspection UI
/// without pulling in a full schema library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortSchema {
    /// JSON Schema `type` keyword (`"object"`, `"array"`, `"string"`, ...).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<String>,
    /// For `object` schemas, the property-name → sub-schema map.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<std::collections::HashMap<String, PortSchema>>,
    /// For `array` schemas, the element schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<PortSchema>>,
    /// For `object` schemas, the list of required property names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    /// Free-form description shown in introspection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Default value supplied when the caller omits this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

/// Flow-level execution policy: timeouts and budgets that wrap the whole
/// step sequence rather than any single step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowConfig {
    /// Hard timeout for the entire flow, in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Human-readable timeout (e.g. `"30s"`) — alternative to `timeout_ms`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// Cap on the number of step transitions to prevent infinite loops.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u64>,
    /// How the runtime should react when a step errors (e.g. `"stop"`,
    /// `"continue"`); interpreted by the executor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_error: Option<String>,
}

/// One row of [`WaferFlow::config_map`]: a user-facing flow-config key is
/// forwarded into `target` block's config under `key`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigMapEntry {
    /// Block name receiving the value (e.g. `"crypto/jwt-sign"`).
    pub target: String,
    /// Key within the target block's config to populate.
    pub key: String,
}

/// Lightweight flow descriptor returned by introspection endpoints —
/// enough to list flows in admin UIs without serializing the full body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowInfo {
    /// Stable flow identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Free-form description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
