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

use super::service::{DatabaseError, Record, RecordData};

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
/// this takes pairs rather than a JSON row. A driver that answers positional
/// rows beside the result's column names (sql.js's `exec`, D1's
/// `raw({ columnNames: true })`) pairs them with [`rows_from_positional`],
/// which is exact. A JS object row is not: JS enumerates integer-like names
/// (`SELECT 1`) before the others, so a backend reads positional rows
/// wherever its driver offers them.
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

/// Pair a positional result — the column names in result order, and each
/// row's values in that order — into rows of name → value, in that order:
/// the input [`record_from_columns`] takes.
///
/// This is how a backend whose driver answers positional rows (sql.js's
/// `exec`, D1's `raw({ columnNames: true })`) keeps a result's column order
/// exactly, integer-like names (`SELECT 1`) included. Two result columns with
/// one name still collapse into one entry, the last one's value, as they do
/// in any [`RecordData`]. A row whose length differs from the column list is
/// not a result the driver produced and is an error rather than truncated.
///
/// # Errors
///
/// [`DatabaseError::Internal`] naming the row's length and the column count.
pub fn rows_from_positional(
    columns: &[String],
    rows: Vec<Vec<serde_json::Value>>,
) -> Result<Vec<RecordData>, DatabaseError> {
    rows.into_iter()
        .map(|row| {
            if row.len() != columns.len() {
                return Err(DatabaseError::Internal(format!(
                    "a result row has {} values for {} columns",
                    row.len(),
                    columns.len()
                )));
            }
            Ok(columns.iter().cloned().zip(row).collect())
        })
        .collect()
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
/// `row` is the first row the statement returned. The shared builders whose
/// answer is read this way (`COUNT`, the id-policy probe, the catalog row
/// count) always return one row holding one integer, so anything else — no
/// row, `NULL`, a non-numeric value, a number with a fractional part or out
/// of `i64` range — is a malformed answer and an
/// [`Internal`](DatabaseError::Internal) error, never a `0`. Native SQLite
/// and PostgreSQL refuse the same answers in their drivers; this keeps a
/// backend that decodes through JSON (D1, sql.js) to the same rule. A number
/// carried as an integral float (a JavaScript number) is that integer.
///
/// # Errors
///
/// [`DatabaseError::Internal`] for any answer but one integer.
pub fn scalar_i64(row: Option<serde_json::Value>) -> Result<i64, DatabaseError> {
    let value = scalar_of(row)?;
    exact_i64(&value).ok_or_else(|| malformed_scalar("an integer", &value))
}

/// The single scalar column of a `SUM`-shaped row as `f64`.
///
/// As [`scalar_i64`]: `SUM` is built `COALESCE`d (`build_sum`), so its row
/// always holds a number, and no row, `NULL` or a non-numeric value is an
/// [`Internal`](DatabaseError::Internal) error rather than `0.0`.
///
/// # Errors
///
/// [`DatabaseError::Internal`] for any answer but one number.
pub fn scalar_f64(row: Option<serde_json::Value>) -> Result<f64, DatabaseError> {
    let value = scalar_of(row)?;
    value
        .as_f64()
        .ok_or_else(|| malformed_scalar("a number", &value))
}

/// Whether a table-existence probe answered "present": the rows of
/// `introspect::build_table_exists` or `introspect::build_catalog_table_exists`,
/// each of which answers exactly one row whose integer column `present` is
/// `1` or `0`.
///
/// Only that answer is definite. Anything else — no row, more than one, a
/// row without `present`, `NULL`, a non-integer, any other number — is an
/// [`Internal`](DatabaseError::Internal) error. A caller may treat
/// `Ok(false)` as proof the table is absent (`impresspress`'s grant loader
/// reads "no grants" from it after a failed read), so a malformed answer must
/// never decode as `false`.
///
/// # Errors
///
/// [`DatabaseError::Internal`] for any answer but one row with `present`
/// `0` or `1`.
pub fn table_presence(rows: &[Record]) -> Result<bool, DatabaseError> {
    let [row] = rows else {
        return Err(DatabaseError::Internal(format!(
            "table-existence probe answered {} rows; expected exactly one",
            rows.len()
        )));
    };
    match row.data.get(PRESENT).map(exact_i64) {
        Some(Some(1)) => Ok(true),
        Some(Some(0)) => Ok(false),
        Some(_) => Err(malformed_scalar(
            "`present` 0 or 1",
            row.data.get(PRESENT).unwrap_or(&serde_json::Value::Null),
        )),
        None => Err(DatabaseError::Internal(
            "table-existence probe answered a row without `present`".to_string(),
        )),
    }
}

/// The column the table-existence probes answer in.
const PRESENT: &str = "present";

/// The scalar of `row`, or the error for a statement that returned no row,
/// or a row with no column.
fn scalar_of(row: Option<serde_json::Value>) -> Result<serde_json::Value, DatabaseError> {
    row.and_then(first_scalar).ok_or_else(|| {
        DatabaseError::Internal("scalar query answered no row; expected one".to_string())
    })
}

/// `value` as an `i64` when it is exactly one: an integer, or a float with
/// no fractional part inside `i64`'s range.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn exact_i64(value: &serde_json::Value) -> Option<i64> {
    // `i64::MIN` is exactly representable; `i64::MAX as f64` rounds up to
    // 2^63, which is out of range, hence the strict upper bound.
    let (min, max) = (i64::MIN as f64, i64::MAX as f64);
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.fract() == 0.0 && *f >= min && *f < max)
            .map(|f| f as i64)
    })
}

fn malformed_scalar(expected: &str, value: &serde_json::Value) -> DatabaseError {
    DatabaseError::Internal(format!(
        "scalar query answered {value}; expected {expected}"
    ))
}
