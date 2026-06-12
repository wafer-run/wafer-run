//! Interface contracts — [`InterfaceSpec`] and per-action [`ActionSpec`].

use std::collections::HashMap;

/// Specification for a block interface — the contract that blocks with
/// this interface must fulfil.
///
/// Describes what the interface does, what actions it handles, and the
/// expected message/response shapes per action.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InterfaceSpec {
    /// Interface identifier, e.g. `"middleware@v1"`.
    pub name: String,
    /// Human-readable description of what blocks with this interface do.
    pub description: String,
    /// Per-action specifications. Key is the action name (e.g. `"retrieve"`,
    /// `"query"`). An empty map means the interface is action-agnostic
    /// (e.g. middleware that passes any message through).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub actions: HashMap<String, ActionSpec>,
}

/// Specification for a single action within an interface.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActionSpec {
    /// What this action does.
    pub description: String,
    /// JSON Schema describing the expected message `data` for this action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_schema: Option<serde_json::Value>,
    /// JSON Schema describing the response `data` for this action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,
}
