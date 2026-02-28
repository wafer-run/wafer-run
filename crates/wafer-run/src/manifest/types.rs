use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// BlockManifest represents a block.json file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockManifest {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageManifest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub services: Option<ManifestServices>,
}

/// MessageManifest declares what a block reads from and writes to the message metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageManifest {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub input: HashMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub output: HashMap<String, serde_json::Value>,
}

/// ManifestServices declares the platform services a block requires.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestServices {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<DatabaseManifest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageManifest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crypto: Option<CryptoManifest>,
}

/// DatabaseManifest declares the database collections a block needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseManifest {
    pub collections: HashMap<String, CollectionDef>,
}

/// StorageManifest declares that the block uses storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageManifest {}

/// CryptoManifest declares that the block uses crypto.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoManifest {}

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
