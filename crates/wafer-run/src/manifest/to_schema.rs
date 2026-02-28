use crate::schema::types::*;
use super::types::*;

/// Convert a block manifest's database collections to schema Table definitions.
pub fn to_schema_tables(m: &BlockManifest) -> Vec<Table> {
    let services = match &m.services {
        Some(s) => s,
        None => return Vec::new(),
    };
    let database = match &services.database {
        Some(d) => d,
        None => return Vec::new(),
    };

    database
        .collections
        .iter()
        .map(|(name, coll)| collection_to_table(name, coll))
        .collect()
}

fn collection_to_table(name: &str, coll: &CollectionDef) -> Table {
    let mut table = Table::new(name);

    for (field_name, field_def) in &coll.fields {
        let col = field_to_column(field_name, field_def);
        table.columns.push(col);
    }

    for idx in &coll.indexes {
        table.indexes.push(Index {
            name: String::new(),
            columns: idx.fields.clone(),
            unique: idx.unique,
        });
    }

    table
}

fn field_to_column(name: &str, f: &FieldDef) -> Column {
    let mut col = Column {
        name: name.to_string(),
        data_type: field_type_to_data_type(&f.field_type),
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

    // Foreign key reference: "table.column"
    if !f.r#ref.is_empty() {
        let parts: Vec<&str> = f.r#ref.splitn(2, '.').collect();
        if parts.len() == 2 {
            col.references = Some(Reference {
                table: parts[0].to_string(),
                column: parts[1].to_string(),
                on_delete: "CASCADE".to_string(),
                on_update: String::new(),
            });
        }
    }

    col
}

fn field_type_to_data_type(t: &str) -> DataType {
    match t.to_lowercase().as_str() {
        "string" => DataType::String,
        "text" => DataType::Text,
        "int" | "integer" => DataType::Int,
        "int64" => DataType::Int64,
        "float" | "number" => DataType::Float,
        "bool" | "boolean" => DataType::Bool,
        "datetime" => DataType::DateTime,
        "json" => DataType::Json,
        "blob" => DataType::Blob,
        _ => DataType::String,
    }
}

fn to_default_value(v: &serde_json::Value, field_type: &str) -> DefaultValue {
    match v {
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
