use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// InterfaceManifest represents a .interface.json file — a reusable JSON Schema
/// type definition that blocks can reference for their input/output contracts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceManifest {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub schema: serde_json::Value,
}

/// BlockManifest represents a block.json file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockManifest {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub collections: HashMap<String, CollectionDef>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub actions: HashMap<String, ActionDef>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub definitions: HashMap<String, serde_json::Value>,
    /// Interface references: alias -> "github.com/owner/repo@version"
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub interfaces: HashMap<String, String>,

    // Legacy fields for backwards compatibility during migration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services: Option<ManifestServices>,
}

/// ActionDef describes a block action with JSON Schema input/output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ActionError>,
    #[serde(default = "default_true_val")]
    pub auth: bool,
}

fn default_true_val() -> bool {
    true
}

/// ActionError describes a possible error response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionError {
    pub code: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// MessageManifest declares what a block reads from and writes to the message metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageManifest {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub input: HashMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub output: HashMap<String, serde_json::Value>,
}

/// ManifestServices declares the platform services a block requires (legacy).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestServices {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<DatabaseManifest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageManifest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crypto: Option<CryptoManifest>,
}

/// DatabaseManifest declares the database collections a block needs (legacy).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseManifest {
    pub collections: HashMap<String, CollectionDef>,
}

/// StorageManifest declares that the block uses storage (legacy).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageManifest {}

/// CryptoManifest declares that the block uses crypto (legacy).
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

impl BlockManifest {
    /// Returns all collections, merging top-level and legacy services.database.collections.
    pub fn all_collections(&self) -> HashMap<String, CollectionDef> {
        let mut result = self.collections.clone();
        if let Some(ref services) = self.services {
            if let Some(ref db) = services.database {
                for (name, coll) in &db.collections {
                    result.entry(name.clone()).or_insert_with(|| coll.clone());
                }
            }
        }
        result
    }

    /// Resolve interface references in action input/output fields.
    ///
    /// Walks all `ActionDef` input/output fields. If a value is a JSON string,
    /// looks it up in the provided `cached_interfaces` map and replaces with the
    /// resolved schema object. Inline JSON Schema objects are left untouched.
    pub fn resolve_interface_refs(
        &mut self,
        cached_interfaces: &HashMap<String, InterfaceManifest>,
    ) {
        for action in self.actions.values_mut() {
            Self::resolve_field(&mut action.input, cached_interfaces);
            Self::resolve_field(&mut action.output, cached_interfaces);
        }
    }

    fn resolve_field(
        field: &mut Option<serde_json::Value>,
        cached_interfaces: &HashMap<String, InterfaceManifest>,
    ) {
        if let Some(serde_json::Value::String(ref key)) = field {
            if let Some(iface) = cached_interfaces.get(key.as_str()) {
                *field = Some(iface.schema.clone());
            }
        }
    }
}
