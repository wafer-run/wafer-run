use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// CollectionDef defines a database collection (table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionDef {
    pub fields: HashMap<String, FieldDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexDef>,
}

/// FieldDef defines a field (column) in a collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub unique: bool,
    #[serde(default)]
    pub optional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub auto: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub r#ref: String,
}

/// IndexDef defines an index on a collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDef {
    pub fields: Vec<String>,
    #[serde(default)]
    pub unique: bool,
}
