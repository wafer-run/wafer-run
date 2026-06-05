use std::collections::HashMap;

use super::types::*;
use crate::error::SchemaError;
use crate::types::*;

/// Convert a map of collection definitions to schema Table definitions.
/// Tables are sorted by name for deterministic creation order (important for
/// foreign key references between tables).
///
/// Returns [`SchemaError`] if any field declares an unknown type or a malformed
/// foreign-key reference, rather than silently coercing the mistake.
pub fn collections_to_tables(
    collections: &HashMap<String, CollectionDef>,
) -> Result<Vec<Table>, SchemaError> {
    let mut names: Vec<_> = collections.keys().collect();
    names.sort();
    names
        .iter()
        .map(|name| collection_to_table(name, &collections[*name]))
        .collect()
}

fn collection_to_table(name: &str, coll: &CollectionDef) -> Result<Table, SchemaError> {
    let mut table = Table::new(name);

    for (field_name, field_def) in &coll.fields {
        let col = field_to_column(name, field_name, field_def)?;
        table.columns.push(col);
    }

    for idx in &coll.indexes {
        table.indexes.push(Index {
            name: String::new(),
            columns: idx.fields.clone(),
            unique: idx.unique,
        });
    }

    Ok(table)
}

fn field_to_column(collection: &str, name: &str, f: &FieldDef) -> Result<Column, SchemaError> {
    let mut col = Column {
        name: name.to_string(),
        data_type: field_type_to_data_type(collection, name, &f.field_type)?,
        primary_key: f.primary,
        unique: f.unique,
        nullable: f.optional,
        auto_increment: false,
        default: None,
        references: None,
    };

    // Auto-increment for integer primary keys
    if f.auto
        && f.primary
        && (f.field_type == "int" || f.field_type == "integer" || f.field_type == "int64")
    {
        col.auto_increment = true;
    }

    // Auto datetime fields default to CURRENT_TIMESTAMP
    if f.auto && f.field_type == "datetime" {
        col.default = Some(default_now());
    }

    // Explicit default values
    if let Some(ref default_val) = f.default {
        if col.default.is_none() {
            col.default = Some(to_default_value(default_val, &f.field_type));
        }
    }

    // Foreign key reference: "table.column". A non-empty ref that lacks the
    // '.' separator cannot be split into table/column, so it is an authoring
    // error rather than a silently-dropped no-op. `split_once` keeps any extra
    // dots in the column part ("a.b.c" → table "a", column "b.c").
    if !f.r#ref.is_empty() {
        match f.r#ref.split_once('.') {
            Some((table, column)) => {
                col.references = Some(Reference {
                    table: table.to_string(),
                    column: column.to_string(),
                    on_delete: "CASCADE".to_string(),
                    on_update: String::new(),
                });
            }
            None => {
                return Err(SchemaError::BadRef {
                    collection: collection.to_string(),
                    field: name.to_string(),
                    reference: f.r#ref.clone(),
                });
            }
        }
    }

    Ok(col)
}

fn field_type_to_data_type(
    collection: &str,
    field: &str,
    t: &str,
) -> Result<DataType, SchemaError> {
    Ok(match t.to_lowercase().as_str() {
        "string" => DataType::String,
        "text" => DataType::Text,
        "int" | "integer" => DataType::Int,
        "int64" => DataType::Int64,
        "float" | "number" => DataType::Float,
        "bool" | "boolean" => DataType::Bool,
        "datetime" => DataType::DateTime,
        "json" => DataType::Json,
        "blob" => DataType::Blob,
        _ => {
            return Err(SchemaError::UnknownFieldType {
                collection: collection.to_string(),
                field: field.to_string(),
                ty: t.to_string(),
            });
        }
    })
}

fn to_default_value(v: &serde_json::Value, field_type: &str) -> DefaultValue {
    match v {
        serde_json::Value::Null => default_null(),
        serde_json::Value::Bool(b) => {
            if *b {
                default_true()
            } else {
                default_false()
            }
        }
        serde_json::Value::Number(n) => {
            if field_type == "int" || field_type == "integer" || field_type == "int64" {
                default_int(n.as_i64().unwrap_or(0))
            } else {
                DefaultValue {
                    raw: String::new(),
                    value: Some(DefaultVal::Float(n.as_f64().unwrap_or(0.0))),
                    is_raw: false,
                    is_null: false,
                }
            }
        }
        serde_json::Value::String(s) => {
            if s == "CURRENT_TIMESTAMP" || s == "NOW()" {
                default_now()
            } else {
                default_string(s.as_str())
            }
        }
        _ => default_string(v.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(field_type: &str) -> FieldDef {
        FieldDef {
            field_type: field_type.to_string(),
            primary: false,
            unique: false,
            optional: false,
            default: None,
            auto: false,
            r#ref: String::new(),
        }
    }

    fn collection_with(fields: &[(&str, FieldDef)]) -> HashMap<String, CollectionDef> {
        let mut map = HashMap::new();
        map.insert(
            "users".to_string(),
            CollectionDef {
                fields: fields
                    .iter()
                    .map(|(n, f)| (n.to_string(), f.clone()))
                    .collect(),
                indexes: Vec::new(),
            },
        );
        map
    }

    #[test]
    fn unknown_field_type_is_rejected() {
        let colls = collection_with(&[("name", field("strnig"))]);
        let err = collections_to_tables(&colls).unwrap_err();
        assert_eq!(
            err,
            SchemaError::UnknownFieldType {
                collection: "users".to_string(),
                field: "name".to_string(),
                ty: "strnig".to_string(),
            }
        );
    }

    #[test]
    fn known_field_types_succeed() {
        let colls = collection_with(&[("id", field("int")), ("body", field("TEXT"))]);
        let tables = collections_to_tables(&colls).unwrap();
        assert_eq!(tables.len(), 1);
        let by_name: HashMap<_, _> = tables[0]
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.data_type))
            .collect();
        assert_eq!(by_name["id"], DataType::Int);
        // Type matching is case-insensitive.
        assert_eq!(by_name["body"], DataType::Text);
    }

    #[test]
    fn malformed_ref_without_dot_is_rejected() {
        let mut f = field("int");
        f.r#ref = "users".to_string(); // no '.column' part
        let colls = collection_with(&[("user_id", f)]);
        let err = collections_to_tables(&colls).unwrap_err();
        assert_eq!(
            err,
            SchemaError::BadRef {
                collection: "users".to_string(),
                field: "user_id".to_string(),
                reference: "users".to_string(),
            }
        );
    }

    #[test]
    fn well_formed_ref_produces_foreign_key() {
        let mut f = field("int");
        f.r#ref = "orgs.id".to_string();
        let colls = collection_with(&[("org_id", f)]);
        let tables = collections_to_tables(&colls).unwrap();
        let col = tables[0]
            .columns
            .iter()
            .find(|c| c.name == "org_id")
            .unwrap();
        let r = col.references.as_ref().expect("expected a foreign key");
        assert_eq!(r.table, "orgs");
        assert_eq!(r.column, "id");
    }

    #[test]
    fn ref_with_extra_dots_keeps_them_in_column() {
        // splitn/split_once semantics: only the first '.' separates table from
        // column, so "a.b.c" → table "a", column "b.c" (accepted, not an error).
        let mut f = field("int");
        f.r#ref = "a.b.c".to_string();
        let colls = collection_with(&[("x", f)]);
        let tables = collections_to_tables(&colls).unwrap();
        let col = tables[0].columns.iter().find(|c| c.name == "x").unwrap();
        let r = col.references.as_ref().expect("expected a foreign key");
        assert_eq!(r.table, "a");
        assert_eq!(r.column, "b.c");
    }
}
