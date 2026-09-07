//! One decode policy for SQL result rows, shared by every SQL-family backend.
//!
//! A SQL backend that reads a row has to answer the same three questions each
//! time: how a column's raw value becomes a [`serde_json::Value`], how a row
//! map becomes a [`Record`], and how a single-column aggregate row becomes a
//! scalar. Each backend used to answer them privately, and they had drifted —
//! most visibly on the JSON-in-TEXT question.
//!
//! # Why JSON-looking TEXT is re-parsed
//!
//! SQLite (and therefore Cloudflare D1 and the browser's sql.js) has no
//! array/object storage class, so the shared write path serializes a
//! [`serde_json::Value::Object`]/[`Array`](serde_json::Value::Array) to its
//! JSON text and binds it as TEXT (see `json_to_sql_value` in
//! `wafer-block-sqlite`, and `build_add_column_for_value`, which gives such a
//! column `TEXT` on SQLite and `JSONB` on Postgres). Without a matching decode
//! the round trip is lossy in a way block code can see: it writes an object and
//! reads back a string.
//!
//! Native SQLite and the browser adapter each re-parsed such a column; the D1
//! adapter did not. Block code that runs on all three therefore saw
//! `Value::Object` on two platforms and `Value::String` on the third. That is
//! the divergence this module removes: one policy, applied by every backend's
//! row decoder, pinned end-to-end by
//! [`run_conformance`](super::conformance::run_conformance).
//!
//! The predicate is deliberately narrow and *untrimmed* — the text must begin
//! with `{` and end with `}` (or `[`/`]`) and parse as JSON. Anything else,
//! including braced text that does not parse, is returned verbatim as a
//! string, so a column holding hand-written text is never lost.

use std::collections::HashMap;

use super::service::Record;

/// Decode one TEXT column value.
///
/// Text that is a serialized JSON object or array is parsed back into the
/// structured value the writer put in; everything else — including malformed
/// JSON and JSON *scalars* such as `42`, `true` or `null`, which the write path
/// never stores as text — stays a [`serde_json::Value::String`]. See the module
/// docs for why.
#[must_use]
pub fn decode_text_value(text: &str) -> serde_json::Value {
    let looks_like_json = (text.starts_with('{') && text.ends_with('}'))
        || (text.starts_with('[') && text.ends_with(']'));
    if looks_like_json {
        if let Ok(parsed) = serde_json::from_str(text) {
            return parsed;
        }
    }
    serde_json::Value::String(text.to_string())
}

/// Convert a result row already shaped as a JSON object (column name → value)
/// into a [`Record`].
///
/// Every string-valued column goes through [`decode_text_value`], so a backend
/// that hands rows over as JSON (Cloudflare D1, the sql.js bridge) decodes them
/// exactly as a backend reading native column values does. `id` is copied into
/// [`Record::id`] **and** retained in [`Record::data`]: row decoders in block
/// repositories consume the complete column map.
///
/// A non-object row yields an empty record — the caller asked for a row shape
/// the backend did not produce, and there is no id to report.
#[must_use]
pub fn record_from_json_row(row: serde_json::Value) -> Record {
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
            serde_json::Value::String(s) => decode_text_value(&s),
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
