//! One value policy for SQL columns, shared by every SQL-family backend.
//!
//! A SQL backend that reads a row has to answer the same three questions each
//! time: how a column's raw value becomes a [`serde_json::Value`], how a row
//! row becomes a [`Record`], and how a single-column aggregate row becomes a
//! scalar. This module answers them once, so the same row reads the same way
//! on native SQLite, PostgreSQL, Cloudflare D1 and the browser's sql.js — and
//! says how a value is written to a JSON column, so it reads back as written.
//!
//! # JSON columns: declared, and holding JSON text
//!
//! A column holds JSON when its declared type says so: `JSON TEXT` (or the
//! older `JSON`) on SQLite, `json`/`jsonb` on Postgres
//! ([`is_json_decl_type`](wafer_sql_utils::introspect::is_json_decl_type)).
//! Every other column holds plain values, and its text is never parsed:
//! guessing from content would turn a user's title `[1]` or `{}` back into an
//! array or object.
//!
//! **Writing.** A JSON column is written with the JSON text of the value
//! ([`encode_json_value`]): an object as `{"a":1}`, the number `123` as `123`,
//! the string `123` as `"123"` (quoted). SQL `NULL` stays `NULL`. The
//! shared executor applies this to every data value it writes to a JSON
//! column, on every backend: SQLite stores the text, Postgres parses it into
//! its `json`/`jsonb` parameter. So any JSON value — a string that looks like
//! a number, a boolean, `null` or JSON included — reads back as the value
//! written. A caller that holds already-serialized JSON parses it and writes
//! the value; there is no "raw JSON text" form.
//!
//! **Reading.** The text of a JSON column is parsed ([`decode_text`]).
//! Postgres returns `json`/`jsonb` columns structured from the driver. SQLite
//! (and therefore D1 and sql.js) returns text, and a backend whose driver
//! reports no column types (D1, sql.js) cannot tell which text is JSON, so the
//! shared executor looks the declared types up (through the
//! [`SchemaCache`](super::schema_cache::SchemaCache)) for the table a
//! statement reads and hands them to the backend's row-returning primitive as
//! [`JsonColumns`]. Native SQLite decodes with them too, so all three agree by
//! construction. A statement with no single source table — raw SQL,
//! aggregates, introspection — is decoded with [`JsonColumns::NONE`]: its text
//! comes back as text (see `DatabaseService::query_raw`).
//!
//! A JSON column whose text does not parse (a value written before the column
//! was declared JSON, say) is returned verbatim as a string rather than lost.

use super::service::{Record, RecordData};

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

/// The value a JSON column is written with: the JSON text of `value`, or SQL
/// `NULL` for `null`. See the module docs.
#[must_use]
pub fn encode_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Null => serde_json::Value::Null,
        other => serde_json::Value::String(other.to_string()),
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

/// Convert a result row — its columns as name → value pairs, **in the order
/// the statement returned them** — into a [`Record`], decoding the text of
/// `json`'s columns.
///
/// Every string-valued column goes through [`decode_text`], so a backend that
/// hands rows over as JS values (Cloudflare D1, the sql.js bridge) decodes
/// them exactly as a backend reading native column values does. `id` is
/// copied into [`Record::id`] **and** retained in [`Record::data`]: row
/// decoders in block repositories consume the complete column map.
///
/// [`Record::data`] keeps the pairs' order (see [`RecordData`]), so a backend
/// must hand them over in result-column order. A `serde_json::Value` object
/// cannot carry that order — `serde_json::Map` sorts its keys — which is why
/// this takes pairs rather than a JSON row: deserialize a driver's row into a
/// [`RecordData`] (a JS object's own keys enumerate in insertion order, which
/// is column order), or zip its column names with a positional row.
#[must_use]
pub fn record_from_columns<I>(row: I, json: &JsonColumns) -> Record
where
    I: IntoIterator<Item = (String, serde_json::Value)>,
{
    let row = row.into_iter();
    let mut data = RecordData::with_capacity(row.size_hint().0);
    let mut id = String::new();
    for (name, value) in row {
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
