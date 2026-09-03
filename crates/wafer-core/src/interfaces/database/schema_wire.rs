//! Wire DTO → `wafer_schema` conversion for the structured schema ops.
//!
//! `wafer-block` (where the wire types live) does not depend on
//! `wafer-schema`, so the mapping lives host-side. Every unknown name is
//! `InvalidArgument`: a schema op never guesses.

use wafer_block::{wire::database as wire, ErrorCode, WaferError};
use wafer_schema::{
    default_now, default_null, Column, DataType, DefaultVal, DefaultValue, Index, Table,
};

fn invalid(msg: String) -> WaferError {
    WaferError::new(ErrorCode::InvalidArgument, msg)
}

pub(crate) fn data_type_from_kind(kind: &str) -> Result<DataType, WaferError> {
    Ok(match kind {
        "string" => DataType::String,
        "text" => DataType::Text,
        "int" => DataType::Int,
        "int64" => DataType::Int64,
        "float" => DataType::Float,
        "bool" => DataType::Bool,
        "datetime" => DataType::DateTime,
        "json" => DataType::Json,
        "blob" => DataType::Blob,
        other => return Err(invalid(format!("unknown column kind `{other}`"))),
    })
}

fn default_from_def(def: &wire::DefaultDef) -> Result<DefaultValue, WaferError> {
    match def.kind.as_str() {
        "null" => Ok(default_null()),
        "now" => Ok(default_now()),
        "value" => {
            let value = match &def.value {
                serde_json::Value::String(s) => DefaultVal::String(s.clone()),
                serde_json::Value::Bool(b) => DefaultVal::Bool(*b),
                serde_json::Value::Number(n) if n.is_i64() => {
                    DefaultVal::Int(n.as_i64().unwrap_or_default())
                }
                serde_json::Value::Number(n) => DefaultVal::Float(n.as_f64().unwrap_or_default()),
                other => return Err(invalid(format!("unsupported default value {other}"))),
            };
            let raw = match &value {
                DefaultVal::String(s) => s.clone(),
                DefaultVal::Int(i) => i.to_string(),
                DefaultVal::Float(f) => f.to_string(),
                DefaultVal::Bool(b) => b.to_string(),
            };
            Ok(DefaultValue {
                raw,
                value: Some(value),
                is_raw: false,
                is_null: false,
            })
        }
        other => Err(invalid(format!("unknown default kind `{other}`"))),
    }
}

pub(crate) fn column_from_def(def: &wire::ColumnDef) -> Result<Column, WaferError> {
    let mut column = Column::new(def.name.clone(), data_type_from_kind(&def.kind)?);
    column.nullable = def.nullable;
    column.primary_key = def.primary_key;
    column.auto_increment = def.auto_increment;
    column.unique = def.unique;
    column.default = def.default.as_ref().map(default_from_def).transpose()?;
    Ok(column)
}

/// Convert a wire [`TableDef`](wire::TableDef) into a [`wafer_schema::Table`].
///
/// `pub` (not `pub(crate)`): exercised directly by
/// `tests/handler_database_schema_ops.rs` to pin the DTO → `wafer_schema`
/// mapping independent of the handler's authorization plumbing. A
/// `pub(crate)` item is invisible to that file — an integration test in
/// `tests/` compiles as a separate crate linked against the ordinary (not
/// `cfg(test)`) build of this library.
pub fn table_from_def(def: &wire::TableDef) -> Result<Table, WaferError> {
    if def.columns.is_empty() {
        return Err(invalid(format!("table `{}` declares no columns", def.name)));
    }
    let mut table = Table::new(def.name.clone());
    table.columns = def
        .columns
        .iter()
        .map(column_from_def)
        .collect::<Result<_, _>>()?;
    table.indexes = def
        .indexes
        .iter()
        .map(|i| Index {
            name: i.name.clone(),
            columns: i.columns.clone(),
            unique: i.unique,
        })
        .collect();
    table.primary_key = def.primary_key.clone();
    table.unique_keys = def.unique_keys.clone();
    Ok(table)
}
