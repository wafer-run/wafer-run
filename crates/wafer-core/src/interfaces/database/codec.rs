//! One decode policy for SQL result rows, shared by every SQL-family backend.
//!
//! A SQL backend that reads a row has to answer the same three questions each
//! time: how a column's raw value becomes a [`serde_json::Value`], how a row
//! map becomes a [`Record`], and how a single-column aggregate row becomes a
//! scalar. This module answers them once, so the same row reads the same way
//! on native SQLite, PostgreSQL, Cloudflare D1 and the browser's sql.js.
//!
//! # Structure comes from the column's declared type
//!
//! SQLite (and therefore D1 and sql.js) has no array/object storage class, so
//! the write path stores an object or array as its JSON text. Whether a text
//! value is JSON to parse back or a string to return as written is decided by
//! the column's declared type, never by what the text looks like: a column
//! declared `JSON` (SQLite) or `json`/`jsonb` (Postgres) holds JSON, and every
//! other column holds plain values. Guessing from content would turn a user's
//! title `[1]` or `{}` back into an array or object.
//!
//! The declared types come from the table's schema
//! ([`build_list_columns`](wafer_sql_utils::introspect::build_list_columns),
//! decided by [`is_json_decl_type`](wafer_sql_utils::introspect::is_json_decl_type)).
//! The shared executor looks them up (through the
//! [`SchemaCache`](super::schema_cache::SchemaCache)) for the table a
//! statement reads and hands them to the backend's row-returning primitive as
//! [`JsonColumns`]; a backend whose driver reports no column types (D1,
//! sql.js) decodes with them, and so does native SQLite, so all three agree by
//! construction. Postgres returns `json`/`jsonb` columns structured from the
//! driver, so a Postgres text column is always a string. A statement with no
//! single source table — raw SQL, aggregates, introspection — is decoded with
//! [`JsonColumns::NONE`]: its text comes back as text.
//!
//! A JSON column whose text does not parse (a value written before the column
//! was declared `JSON`, say) is returned verbatim as a string rather than lost.

use std::collections::HashMap;

use super::service::Record;

/// The columns of a result row that hold JSON text, by name.
///
/// Built from a table's declared column types; see the module docs. Names
/// compare ASCII case-insensitively, as SQL identifiers do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonColumns(Vec<String>);

impl JsonColumns {
    /// No JSON columns: every text value decodes as a string.
    pub const NONE: &'static Self = &Self(Vec::new());

    /// The JSON columns named in `names`.
    #[must_use]
    pub fn new<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self(names.into_iter().map(Into::into).collect())
    }

    /// Whether `column` holds JSON text.
    #[must_use]
    pub fn contains(&self, column: &str) -> bool {
        self.0.iter().any(|c| c.eq_ignore_ascii_case(column))
    }

    /// Whether no column holds JSON text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Decode the text value of column `column`.
///
/// In a JSON column the text is parsed; text that does not parse stays a
/// [`serde_json::Value::String`]. In any other column the text is returned
/// as a string, whatever it looks like.
#[must_use]
pub fn decode_text(column: &str, text: &str, json: &JsonColumns) -> serde_json::Value {
    if json.contains(column) {
        if let Ok(parsed) = serde_json::from_str(text) {
            return parsed;
        }
    }
    serde_json::Value::String(text.to_string())
}

/// Convert a result row already shaped as a JSON object (column name → value)
/// into a [`Record`], decoding the text of `json`'s columns.
///
/// Every string-valued column goes through [`decode_text`], so a backend that
/// hands rows over as JSON (Cloudflare D1, the sql.js bridge) decodes them
/// exactly as a backend reading native column values does. `id` is copied into
/// [`Record::id`] **and** retained in [`Record::data`]: row decoders in block
/// repositories consume the complete column map.
///
/// A non-object row yields an empty record — the caller asked for a row shape
/// the backend did not produce, and there is no id to report.
#[must_use]
pub fn record_from_json_row(row: serde_json::Value, json: &JsonColumns) -> Record {
    let serde_json::Value::Object(map) = row else {
        return Record {
            id: String::new(),
            data: HashMap::new(),
        };
    };

    let mut data: HashMap<String, serde_json::Value> = HashMap::with_capacity(map.len());
    let mut id = String::new();
    for (name, value) in map {
        let value = match value {
            serde_json::Value::String(s) => decode_text(&name, &s, json),
            other => other,
        };
        if name == "id" {
            id = record_id(&value);
        }
        data.insert(name, value);
    }
    Record { id, data }
}

/// The textual [`Record::id`] for an already-decoded `id` column value.
///
/// A string is taken verbatim and an integer key is stringified; anything else
/// (NULL, or a column that decoded to a structured value) has no textual id and
/// yields the empty string, which is what every backend's hand-written decoder
/// did.
#[must_use]
pub fn record_id(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The single value of a one-column result row, whatever the column is aliased
/// as.
///
/// The shared statement builders alias their scalar column themselves
/// (`build_count` and friends), so callers cannot look the value up by name.
/// A row that is already a bare scalar is returned as-is.
#[must_use]
pub fn first_scalar(row: serde_json::Value) -> Option<serde_json::Value> {
    match row {
        serde_json::Value::Object(map) => map.into_iter().next().map(|(_, v)| v),
        other => Some(other),
    }
}

/// The single scalar column of a `COUNT`-shaped row as `i64`.
///
/// A missing row or a non-numeric value is `0` — the same answer the SQL
/// aggregate itself gives for an empty table, so an absent row cannot be
/// mistaken for a count.
#[must_use]
pub fn scalar_i64(row: Option<serde_json::Value>) -> i64 {
    row.and_then(first_scalar)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f.trunc() as i64)))
        .unwrap_or(0)
}

/// The single scalar column of a `SUM`-shaped row as `f64`. `0.0` when absent,
/// for the same reason as [`scalar_i64`].
#[must_use]
pub fn scalar_f64(row: Option<serde_json::Value>) -> f64 {
    row.and_then(first_scalar)
        .and_then(|v| {
            #[allow(clippy::cast_precision_loss)]
            v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
        })
        .unwrap_or(0.0)
}
