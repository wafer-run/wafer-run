//! Convert `BlockInfo.collections` (Vec<CollectionSchema>) into the existing
//! manifest `CollectionDef` types, which can then be converted to schema `Table`
//! types via `collections_to_tables()`.
//!
//! This bridges the block-declared schema → database table creation pipeline.

use std::collections::HashMap;
use wafer_block::{CollectionSchema, FieldSchema};
use super::types::{CollectionDef, FieldDef, IndexDef};

/// Convert a block's `CollectionSchema` declarations into manifest `CollectionDef`
/// types keyed by collection name.
///
/// The resulting HashMap can be passed to `collections_to_tables()` to generate
/// SQL-ready `Table` definitions.
pub fn block_collections_to_defs(
    collections: &[CollectionSchema],
) -> HashMap<String, CollectionDef> {
    let mut map = HashMap::new();
    for coll in collections {
        let mut fields = HashMap::new();

        // Every collection gets an auto-generated TEXT primary key `id`.
        fields.insert("id".to_string(), FieldDef {
            field_type: "string".to_string(),
            primary: true,
            unique: false,
            optional: false,
            default: None,
            auto: false,
            r#ref: String::new(),
        });

        // Every collection gets auto-set created_at / updated_at.
        for ts in &["created_at", "updated_at"] {
            fields.insert(ts.to_string(), FieldDef {
                field_type: "datetime".to_string(),
                primary: false,
                unique: false,
                optional: false,
                default: Some(serde_json::Value::String("CURRENT_TIMESTAMP".to_string())),
                auto: true,
                r#ref: String::new(),
            });
        }

        // User-declared fields.
        for f in &coll.fields {
            fields.insert(f.name.clone(), field_schema_to_def(f));
        }

        let indexes = coll.indexes.iter().map(|i| IndexDef {
            fields: i.fields.clone(),
            unique: i.unique,
        }).collect();

        map.insert(coll.name.clone(), CollectionDef { fields, indexes });
    }
    map
}

fn field_schema_to_def(f: &FieldSchema) -> FieldDef {
    let default = if f.default_value.is_empty() {
        None
    } else {
        // Try to parse as JSON value, fall back to string
        match serde_json::from_str::<serde_json::Value>(&f.default_value) {
            Ok(v) => Some(v),
            Err(_) => Some(serde_json::Value::String(f.default_value.clone())),
        }
    };

    FieldDef {
        field_type: f.field_type.clone(),
        primary: false,
        unique: f.unique,
        optional: f.optional,
        default,
        auto: false,
        r#ref: f.reference.clone(),
    }
}

/// Convenience: convert a block's collections directly to schema Tables.
pub fn block_collections_to_tables(
    collections: &[CollectionSchema],
) -> Vec<crate::schema::Table> {
    let defs = block_collections_to_defs(collections);
    super::to_schema::collections_to_tables(&defs)
}
