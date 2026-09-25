//! Shared SQL-backend execution layer behind [`DatabaseService`].
//!
//! SQL backends (SQLite, Postgres) implement the small set of execution
//! *primitives* on [`DbExec`]; the default methods provide the orchestration
//! that is identical across SQL backends. Each backend's
//! [`DatabaseService`](super::service::DatabaseService) impl forwards the
//! shared methods into these defaults.
//!
//! `DbExec` is intentionally **not** object-safe (it carries `const BACKEND`).
//! Services are stored as `Arc<dyn DatabaseService>`, never `dyn DbExec`, so
//! object safety is not required. A blanket
//! `impl<T: DbExec> DatabaseService for T` is impossible: multiple concrete
//! `DatabaseService` impls exist (SQLite, Postgres, browser, D1, test mocks),
//! and Rust coherence (E0119) forbids a blanket impl alongside them.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterTree, ListOptions, SortField};
use wafer_block_macro::wafer_async_trait;
use wafer_sql_utils::{
    ddl, guard,
    ident::validate_ident,
    introspect::{self, IdPolicy},
    value::sea_values_to_json,
    Backend,
};

use super::{
    codec::{self, encode_json_value, JsonColumns},
    schema_cache::{SchemaCache, TableColumns},
    service::{
        AggregateSpec, CapGuard, DatabaseError, GuardedInsert, GuardedUpdate, Record, RecordList,
        StatementBudget, UpsertConflict, UpsertSpec, WriteOp, WriteOutcome,
    },
};

/// `name` as the executor puts it in SQL: verbatim, once it passes
/// [`validate_ident`]. A table or column name is never rewritten — stripping
/// characters would turn one name into another (`a-b` into `ab`) and send the
/// statement to a table or column the caller never named.
fn sql_name(name: &str) -> Result<&str, DatabaseError> {
    Ok(validate_ident(name)?)
}

/// Sort `data` into deterministic `(column, value)` pairs as they are
/// written, refusing a key that is not a plain identifier (see [`sql_name`]).
/// A value for one of `json`'s columns is written as its JSON text
/// ([`codec::encode_json_value`](super::codec::encode_json_value)), so it
/// reads back as the value written.
///
/// Sorted-key iteration keeps the generated INSERT/UPDATE shape stable across
/// process starts: `HashMap` order is randomized by `RandomState`, which would
/// otherwise produce N permutations of the same statement — each a distinct
/// cached prepared statement on the backend.
fn sorted_pairs(
    data: &HashMap<String, serde_json::Value>,
    json: &JsonColumns,
) -> Result<Vec<(String, serde_json::Value)>, DatabaseError> {
    let mut pairs: Vec<(String, serde_json::Value)> = data
        .iter()
        .map(|(k, v)| {
            let value = if json.contains(k) {
                encode_json_value(v)
            } else {
                v.clone()
            };
            Ok((sql_name(k)?.to_string(), value))
        })
        .collect::<Result<_, DatabaseError>>()?;
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(pairs)
}

/// Every column a read or a filtered write names: the `filters` fields, the
/// `sort` fields, every column the `filter_tree` names (see
/// [`tree_leaf_fields`]) and the `projection`. Checked by
/// [`DbExec::require_columns`] before the statement runs.
fn query_columns<'a>(
    filters: &'a [Filter],
    sort: &'a [SortField],
    filter_tree: Option<&'a [FilterTree]>,
    projection: Option<&'a [String]>,
) -> Vec<&'a str> {
    filters
        .iter()
        .map(|f| f.field.as_str())
        .chain(sort.iter().map(|s| s.field.as_str()))
        .chain(filter_tree.map(tree_leaf_fields).unwrap_or_default())
        .chain(projection.unwrap_or_default().iter().map(String::as_str))
        .collect()
}

/// The columns every guard's filters name, and each `SumAtMost` field.
fn guard_columns(guards: &[CapGuard]) -> Vec<&str> {
    guards
        .iter()
        .flat_map(|guard| match guard {
            CapGuard::CountBelow { filters, .. } => query_columns(filters, &[], None, None),
            CapGuard::SumAtMost { field, filters, .. } => {
                let mut columns = query_columns(filters, &[], None, None);
                columns.push(field.as_str());
                columns
            }
        })
        .collect()
}

/// Recursively collect every column a [`FilterTree`] names — the `field` of
/// each [`FilterTree::Leaf`], and both columns of each
/// [`FilterTree::ColumnCompare`] — depth-first, so a field that appears only
/// inside a group (`All`/`Any`) is checked as a flat filter's is.
fn tree_leaf_fields(nodes: &[FilterTree]) -> Vec<&str> {
    fn walk<'a>(node: &'a FilterTree, out: &mut Vec<&'a str>) {
        match node {
            FilterTree::Leaf(f) => out.push(f.field.as_str()),
            FilterTree::ColumnCompare(f) => {
                out.push(f.field.as_str());
                out.push(f.column.as_str());
            }
            FilterTree::All(children) | FilterTree::Any(children) => {
                for child in children {
                    walk(child, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    for node in nodes {
        walk(node, &mut out);
    }
    out
}

/// Mint the `id` of a record created without one: a UUIDv7.
///
/// This is the record-id policy for every [`DbExec`] backend. The shared
/// [`create`](DbExec::create), [`create_many`](DbExec::create_many) and
/// [`batch`](DbExec::batch) call it, and a backend that inserts rows through its own path (a native batch
/// API, say) must call it too, so every backend's ids sort the same way.
///
/// A v7 id leads with its creation time in milliseconds and, within one
/// process, [`Uuid::now_v7`](uuid::Uuid::now_v7) keeps ids strictly
/// increasing, so key order is creation order. That matters because `list`
/// breaks ties on the primary key: rows whose sort key ties (a `created_at`
/// stamped in the same millisecond, as wasm32's `Date.now()` clock does for a
/// burst of inserts) come back in the order they were created. A random v4 id
/// would shuffle them.
///
/// Across processes (two Workers isolates, say) ids minted in the same
/// millisecond are ordered by their random tail, not by creation; nothing
/// shared orders those rows anyway.
///
/// On `wasm32-unknown-unknown` the clock is `Date.now()` through uuid's `js`
/// feature, the same feature the embedding binary already enables for its
/// randomness source; without it `std::time::SystemTime` panics there.
///
/// A v7 id reveals when its record was created and carries about 74 random
/// bits, and ids minted in one millisecond are near-sequential. A record id
/// is therefore guessable from its neighbours and must never serve as a
/// bearer secret (a share link, a reset token); mint those separately.
#[must_use]
pub fn mint_record_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// The current instant in the one text form the executor writes every
/// timestamp column in: RFC 3339, UTC (`2026-09-24T10:00:00.123456789+00:00`).
///
/// Values of this form order correctly as text on the SQLite family, and the
/// Postgres backend binds them into a `TIMESTAMPTZ` column as a timestamp, so
/// a caller can compare a stored stamp against any RFC 3339 cutoff.
fn timestamp_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Stamp `updated_at` (and on create, `created_at`) if the caller didn't.
fn stamp_timestamps(data: &mut HashMap<String, serde_json::Value>, include_created: bool) {
    let now = timestamp_now();
    if include_created && !data.contains_key("created_at") {
        data.insert(
            "created_at".to_string(),
            serde_json::Value::String(now.clone()),
        );
    }
    if !data.contains_key("updated_at") {
        data.insert("updated_at".to_string(), serde_json::Value::String(now));
    }
}

/// Apply [`DbExec::create`]'s per-row policy to a row for `table`, then stamp
/// the timestamps. `policy` is the table's [`IdPolicy`] when the row carries
/// no `id`, `None` when it does: [`IdPolicy::Mint`] mints one,
/// [`IdPolicy::Database`] leaves it to the insert, and [`IdPolicy::Caller`]
/// refuses the row with [`DatabaseError::InvalidArgument`] — a minted string
/// does not belong in an integer `id` that nothing fills.
fn prepare_created_row(
    table: &str,
    data: &mut HashMap<String, serde_json::Value>,
    policy: Option<IdPolicy>,
) -> Result<(), DatabaseError> {
    match policy {
        Some(IdPolicy::Mint) => {
            data.insert(
                "id".to_string(),
                serde_json::Value::String(mint_record_id()),
            );
        }
        Some(IdPolicy::Caller) => {
            return Err(DatabaseError::InvalidArgument(format!(
                "`{table}` declares an integer `id` that the database does not fill: \
                 give the row an `id`, or declare the key so the database numbers \
                 rows (SQLite `id INTEGER PRIMARY KEY`, Postgres an identity or \
                 serial column)"
            )));
        }
        Some(IdPolicy::Database) | None => {}
    }
    stamp_timestamps(data, true);
    Ok(())
}

/// The insert values of a windowed-counter upsert, read out of its `data`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowedCounterRow<'a> {
    /// The row id a brand-new counter row is inserted with.
    pub id: &'a str,
    /// The UNIQUE column the counter row is keyed by (the conflict target).
    pub conflict_column: &'a str,
    /// The value stored in `conflict_column`.
    pub conflict_value: &'a str,
}

/// Read a windowed-counter upsert's insert values out of its `data` and
/// `conflict_columns`, refusing any shape whose parts the statement would not
/// write.
///
/// The statement writes one row keyed by one UNIQUE column, and takes exactly
/// two values from `data`: `id` and the conflict column's value, both strings.
/// The counter, window and timestamp columns are written by the statement
/// itself. So `conflict_columns` must name exactly one column other than `id`,
/// and `data` must hold a string for `id`, a string for that column, and
/// nothing else. Anything else is [`DatabaseError::InvalidArgument`]: a second
/// conflict column or another data field would otherwise be dropped without a
/// word.
///
/// The database handler checks a wire request with this before it reaches a
/// backend, and [`DbExec::upsert`] checks every spec with it again, so a
/// caller of the service cannot get past it by skipping the handler.
pub fn windowed_counter_row<'a>(
    data: &'a [(String, serde_json::Value)],
    conflict_columns: &'a [String],
) -> Result<WindowedCounterRow<'a>, DatabaseError> {
    let conflict_column = match conflict_columns {
        [only] if only != "id" => only.as_str(),
        [_] => {
            return Err(DatabaseError::InvalidArgument(
                "windowed-counter upsert cannot key the counter by `id`: `id` is the \
                 per-call row id, not the counter's key"
                    .into(),
            ))
        }
        _ => {
            return Err(DatabaseError::InvalidArgument(format!(
                "windowed-counter upsert requires exactly one conflict column, got {}",
                conflict_columns.len()
            )))
        }
    };
    let mut id = None;
    let mut conflict_value = None;
    for (column, value) in data {
        let slot = if column == "id" {
            &mut id
        } else if column == conflict_column {
            &mut conflict_value
        } else {
            return Err(DatabaseError::InvalidArgument(format!(
                "windowed-counter upsert writes only `id` and the conflict column \
                 `{conflict_column}` from data; `{column}` would not be written"
            )));
        };
        if slot.is_some() {
            return Err(DatabaseError::InvalidArgument(format!(
                "windowed-counter upsert data names `{column}` twice"
            )));
        }
        *slot = Some(value.as_str().ok_or_else(|| {
            DatabaseError::InvalidArgument(format!(
                "windowed-counter upsert requires a string `{column}` value in data"
            ))
        })?);
    }
    let missing = |column: &str| {
        DatabaseError::InvalidArgument(format!(
            "windowed-counter upsert requires a string `{column}` value in data"
        ))
    };
    Ok(WindowedCounterRow {
        id: id.ok_or_else(|| missing("id"))?,
        conflict_column,
        conflict_value: conflict_value.ok_or_else(|| missing(conflict_column))?,
    })
}

/// One statement in a [`DbExec::run_batch`] call, tagged with how its result
/// should be decoded.
///
/// `sql` + `params` are exactly what the single-statement primitives take:
/// `params` is the JSON form produced by
/// [`sea_values_to_json`](wafer_sql_utils::value::sea_values_to_json)`(stmt.values)`.
/// Each variant names the primitive whose decoding it mirrors, so a batching
/// backend can decode each statement's result the same way the corresponding
/// single-statement primitive would.
#[derive(Clone, Copy, Debug)]
pub enum BatchOp<'a> {
    /// Row-returning; decode every row to a [`Record`] (like
    /// [`DbExec::run_fetch`]).
    Rows {
        /// Rendered SQL for this statement.
        sql: &'a str,
        /// Positional parameters, JSON-encoded as `sea_values_to_json` produces.
        params: &'a [serde_json::Value],
        /// Columns whose text is JSON, as the primitive's `json` argument.
        json: &'a JsonColumns,
    },
    /// Expected to return exactly one row; no rows → `NotFound` (like
    /// [`DbExec::run_fetch_one`]).
    FetchOne {
        /// Rendered SQL for this statement.
        sql: &'a str,
        /// Positional parameters, JSON-encoded as `sea_values_to_json` produces.
        params: &'a [serde_json::Value],
        /// Columns whose text is JSON, as the primitive's `json` argument.
        json: &'a JsonColumns,
    },
    /// Non-row statement; yields the affected-row count (like
    /// [`DbExec::run_execute`]).
    Execute {
        /// Rendered SQL for this statement.
        sql: &'a str,
        /// Positional parameters, JSON-encoded as `sea_values_to_json` produces.
        params: &'a [serde_json::Value],
    },
    /// Single `i64` scalar, e.g. `COUNT(*)` (like [`DbExec::run_scalar_i64`]).
    ScalarI64 {
        /// Rendered SQL for this statement.
        sql: &'a str,
        /// Positional parameters, JSON-encoded as `sea_values_to_json` produces.
        params: &'a [serde_json::Value],
    },
    /// Single `f64` scalar, e.g. `SUM(...)` (like [`DbExec::run_scalar_f64`]).
    ScalarF64 {
        /// Rendered SQL for this statement.
        sql: &'a str,
        /// Positional parameters, JSON-encoded as `sea_values_to_json` produces.
        params: &'a [serde_json::Value],
    },
}

impl<'a> BatchOp<'a> {
    /// The `(sql, params)` pair every variant carries, for backends that
    /// prepare each statement uniformly before dispatching to a native
    /// multi-statement API (the D1 override in the impresspress consumer).
    #[must_use]
    pub fn sql_params(&self) -> (&'a str, &'a [serde_json::Value]) {
        match *self {
            BatchOp::Rows { sql, params, .. }
            | BatchOp::FetchOne { sql, params, .. }
            | BatchOp::Execute { sql, params }
            | BatchOp::ScalarI64 { sql, params }
            | BatchOp::ScalarF64 { sql, params } => (sql, params),
        }
    }
}

/// The decoded result of one [`BatchOp`], returned in the same position as the
/// op that produced it.
#[derive(Debug)]
pub enum BatchResult {
    /// Rows decoded from a [`BatchOp::Rows`].
    Rows(Vec<Record>),
    /// The single row of a [`BatchOp::FetchOne`].
    FetchOne(Record),
    /// Affected-row count of a [`BatchOp::Execute`].
    Execute(i64),
    /// Scalar of a [`BatchOp::ScalarI64`].
    ScalarI64(i64),
    /// Scalar of a [`BatchOp::ScalarF64`].
    ScalarF64(f64),
}

/// A [`run_batch`](DbExec::run_batch) result whose variant doesn't line up with
/// the [`BatchOp`] submitted at that position is an internal invariant
/// violation: the sequential default preserves order and variant, and any
/// batching override must too. Surface it as `Internal` rather than panicking.
fn batch_shape_error(what: &str, got: Option<&BatchResult>) -> DatabaseError {
    DatabaseError::Internal(format!(
        "run_batch returned an unexpected result shape for {what}: {got:?}"
    ))
}

/// A failed schema step (`CREATE TABLE`, `ADD COLUMN`, `CREATE INDEX`) with
/// `what` naming it. The failure is the backend's, never the caller's, so it is
/// `Internal` — unless it is transient: a busy database or a dropped connection
/// stays [`DatabaseError::Unavailable`], so a block Init migrating its schema
/// is retried rather than failed for good.
fn schema_step_error(what: &str, e: DatabaseError) -> DatabaseError {
    match e {
        DatabaseError::Unavailable(msg) => DatabaseError::Unavailable(format!("{what}: {msg}")),
        other => DatabaseError::Internal(format!("{what}: {other}")),
    }
}

/// The index of the first guard a [`guard::build_guard_probe`] result says
/// refuses, or `None` when all `guards` guards hold.
fn first_refusing_guard(probe: TxResult, guards: usize) -> Result<Option<usize>, DatabaseError> {
    let verdicts = match probe {
        TxResult::Returning(rows) => rows
            .into_iter()
            .next()
            .ok_or_else(|| DatabaseError::Internal("the guard probe returned no row".into()))?,
        other @ TxResult::Execute(_) => {
            return Err(DatabaseError::Internal(format!(
                "run_transaction returned {other:?} for the guard probe"
            )))
        }
    };
    for index in 0..guards {
        let column = guard::guard_probe_column(index);
        match verdicts
            .data
            .get(&column)
            .and_then(serde_json::Value::as_i64)
        {
            Some(1) => {}
            Some(0) => return Ok(Some(index)),
            _ => {
                return Err(DatabaseError::Internal(format!(
                    "the guard probe's {column} is not 0 or 1: {:?}",
                    verdicts.data
                )))
            }
        }
    }
    Ok(None)
}

/// One statement in a [`DbExec::run_transaction`] call.
///
/// `sql` + `params` are what the single-statement primitives take: `params` is
/// the JSON form [`sea_values_to_json`]`(stmt.values)` produces.
#[derive(Clone, Copy, Debug)]
pub enum TxOp<'a> {
    /// A write whose affected-row count is the result (like
    /// [`DbExec::run_execute`]).
    Execute {
        /// Rendered SQL for this statement.
        sql: &'a str,
        /// Positional parameters, JSON-encoded as `sea_values_to_json` produces.
        params: &'a [serde_json::Value],
    },
    /// A write that returns rows (`… RETURNING *`), decoded to [`Record`]s
    /// (like [`DbExec::run_execute_returning`]).
    Returning {
        /// Rendered SQL for this statement.
        sql: &'a str,
        /// Positional parameters, JSON-encoded as `sea_values_to_json` produces.
        params: &'a [serde_json::Value],
        /// Columns whose text is JSON, as the primitive's `json` argument.
        json: &'a JsonColumns,
    },
}

impl<'a> TxOp<'a> {
    /// The `(sql, params)` pair every variant carries.
    #[must_use]
    pub fn sql_params(&self) -> (&'a str, &'a [serde_json::Value]) {
        match *self {
            TxOp::Execute { sql, params } | TxOp::Returning { sql, params, .. } => (sql, params),
        }
    }
}

/// The result of one [`TxOp`], returned in the same position as the op.
#[derive(Debug)]
pub enum TxResult {
    /// Affected-row count of a [`TxOp::Execute`].
    Execute(i64),
    /// Rows returned by a [`TxOp::Returning`].
    Returning(Vec<Record>),
}

/// Execution primitives + shared orchestration for SQL `DatabaseService` backends.
#[wafer_async_trait]
pub trait DbExec: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// SQL dialect this backend builds for (placeholder style, introspection).
    const BACKEND: Backend;

    // ---- Backend configuration accessors (defaulted; SQL backends override) ----

    /// Per-backend schema-introspection cache, if the backend keeps one.
    ///
    /// SQL backends return `Some` so the shared table-exists and column-list
    /// paths memoize introspection instead of issuing a round-trip per logical
    /// operation. The default `None` keeps every other implementor — and any
    /// backend that can't cache — on the always-introspect path, unchanged.
    fn schema_cache(&self) -> Option<&SchemaCache> {
        None
    }

    /// Whether the backend trusts its migrated schema (STRICT_SCHEMA mode,
    /// `WAFER_RUN__DATABASE__STRICT_SCHEMA`).
    ///
    /// When `true`, the shared orchestration skips the per-operation
    /// table-exists guard (migrations are authoritative — the table is assumed
    /// present), the write path's lazy column-add `ALTER TABLE` (the migrated
    /// schema is trusted — no columns are synthesized) and the column check a
    /// read or filtered write runs ([`require_columns`](Self::require_columns);
    /// the backend's own "no such column" error answers instead). The one
    /// introspection left is the primary-key lookup a sorted or paged
    /// [`list`](Self::list) orders by
    /// ([`get_primary_key`](Self::get_primary_key)), which a backend with a
    /// [`schema_cache`](Self::schema_cache) issues once per table, plus one
    /// existence probe for a table whose key comes back empty. Default
    /// `false` keeps the introspecting behaviour — a write adds the columns
    /// its data names — for development, tests, and other implementors.
    fn strict_schema(&self) -> bool {
        false
    }

    /// The statements this backend may still run in the current invocation
    /// (see [`StatementBudget`]).
    ///
    /// The shared orchestration admits every
    /// [`run_transaction`](Self::run_transaction) against it after planning,
    /// so the schema introspection a write ran while planning is already
    /// counted and a transaction the backend could not finish is refused
    /// before its first statement. A backend with a per-invocation limit (D1)
    /// counts the statements it issues and reports `Limited`; one without
    /// (native SQLite, PostgreSQL) reports `Unbounded`.
    ///
    /// No default, for the reason
    /// [`DatabaseService::statement_budget`](super::service::DatabaseService::statement_budget)
    /// has none: a backend with a limit that inherited `Unbounded` would
    /// fail its writes part-way through a request.
    fn statement_budget(&self) -> Result<StatementBudget, DatabaseError>;

    // ---- Primitives: the only backend-specific execution code ----
    // `params` is the JSON form produced by `sea_values_to_json(stmt.values)`;
    // each backend binds it natively. All callers pass single-statement SQL.
    // `json` names the result columns whose text is JSON: a row-returning
    // primitive decodes every row with it (`codec::decode_text` /
    // `codec::record_from_json_row`) and never guesses from content. The
    // executor derives it from the declared types of the table a statement
    // reads ([`json_columns`](Self::json_columns)); a statement with no single
    // source table passes `JsonColumns::NONE`.

    /// Run a row-returning query and convert rows to `Record`s, decoding the
    /// text of `json`'s columns as JSON.
    ///
    /// Read path: the statement must have no side effects (a plain `SELECT`).
    /// Implementors that route work along separate read/write paths (e.g.
    /// dedicated reader connections) serve this from the read path, so a
    /// write — including a `… RETURNING` statement — passed here is not
    /// applied. Implementors must surface such a statement's failure as an
    /// `Err`, never as an empty `Ok`: a caller can never be allowed to mistake
    /// a rejected write for a read that found nothing. Use
    /// [`run_execute_returning`](Self::run_execute_returning) for a statement
    /// that has side effects and also returns rows.
    async fn run_fetch(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError>;

    /// Run a query expected to return exactly one row; no rows → `NotFound`.
    ///
    /// Read path: same contract as [`run_fetch`](Self::run_fetch) — no side
    /// effects.
    async fn run_fetch_one(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Record, DatabaseError>;

    /// Run a non-row statement; returns the affected-row count.
    ///
    /// Write path.
    async fn run_execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError>;

    /// Run a write statement that returns rows (`… RETURNING`); runs on the
    /// write path.
    ///
    /// Write path: the same path as [`run_execute`](Self::run_execute), not
    /// [`run_fetch`](Self::run_fetch). Any statement with side effects that
    /// also needs its rows back (`DELETE … RETURNING`, `UPDATE … RETURNING`,
    /// `INSERT … RETURNING`) must go through this primitive rather than
    /// `run_fetch`, so a backend with dedicated read-only connections still
    /// applies the write.
    async fn run_execute_returning(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError>;

    /// Run a query returning a single `i64` scalar (e.g. `COUNT(*)`).
    async fn run_scalar_i64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError>;

    /// Run a query returning a single `f64` scalar (e.g. `SUM(...)`).
    async fn run_scalar_f64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<f64, DatabaseError>;

    /// Whether `table` exists (already-sanitized or raw name, per call site).
    async fn dbx_table_exists(&self, table: &str) -> Result<bool, DatabaseError>;

    /// Run `ops` as ONE transaction on the write path, returning one
    /// [`TxResult`] per op in the same order.
    ///
    /// All or nothing: when a statement fails, every statement before it is
    /// rolled back and that failure is returned. The statements run in order
    /// on one connection, so each sees the writes of the ones before it, and
    /// no other writer's statements interleave with them.
    ///
    /// No default. Running the statements one by one, as
    /// [`run_batch`](Self::run_batch)'s default does, would leave the earlier
    /// ones applied when a later one fails — exactly what
    /// [`create_many`](Self::create_many) and [`batch`](Self::batch) promise
    /// not to do. A backend with a native atomic multi-statement API (D1's
    /// `batch()`) implements this with it.
    async fn run_transaction(&self, ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, DatabaseError>;

    /// Run `ops` as one backend round-trip when the backend can, returning one
    /// [`BatchResult`] per op **in the same order**.
    ///
    /// The default executes each op sequentially by dispatching to the
    /// single-statement primitives ([`run_fetch`](Self::run_fetch),
    /// [`run_fetch_one`](Self::run_fetch_one), [`run_execute`](Self::run_execute),
    /// [`run_scalar_i64`](Self::run_scalar_i64),
    /// [`run_scalar_f64`](Self::run_scalar_f64)) — behaviourally identical to
    /// issuing the statements one-by-one, which is exactly what every backend
    /// does today. So sqlite, postgres, the browser backend, and the test mocks
    /// inherit it unchanged (still N awaits, same order, first failing op
    /// aborts the whole batch and propagates its error). Only a backend with a
    /// native multi-statement API (D1 `batch()`) overrides this to collapse the
    /// round-trips; such a backend must preserve positional alignment and the
    /// same all-or-nothing failure semantics.
    async fn run_batch(&self, ops: &[BatchOp<'_>]) -> Result<Vec<BatchResult>, DatabaseError> {
        let mut out = Vec::with_capacity(ops.len());
        for &op in ops {
            let result = match op {
                BatchOp::Rows { sql, params, json } => {
                    BatchResult::Rows(self.run_fetch(sql, params, json).await?)
                }
                BatchOp::FetchOne { sql, params, json } => {
                    BatchResult::FetchOne(self.run_fetch_one(sql, params, json).await?)
                }
                BatchOp::Execute { sql, params } => {
                    BatchResult::Execute(self.run_execute(sql, params).await?)
                }
                BatchOp::ScalarI64 { sql, params } => {
                    BatchResult::ScalarI64(self.run_scalar_i64(sql, params).await?)
                }
                BatchOp::ScalarF64 { sql, params } => {
                    BatchResult::ScalarF64(self.run_scalar_f64(sql, params).await?)
                }
            };
            out.push(result);
        }
        Ok(out)
    }

    // ---- Shared default methods (the dedup'd orchestration) ----
    // Named like the `DatabaseService` methods; the `DatabaseService` impl
    // forwards with explicit qualification (`DbExec::get(self, ...)`) to avoid
    // self-recursion.

    /// Cache-consulting hot-path existence guard: `true` if the operation
    /// should proceed against `table`.
    ///
    /// In STRICT_SCHEMA mode always `true` — migrations are authoritative, so
    /// the table is assumed present and no probe is issued. Otherwise `true`
    /// without a probe when the [`schema_cache`](Self::schema_cache) knows the
    /// table exists, else one [`dbx_table_exists`](Self::dbx_table_exists)
    /// probe, whose answer is memoized only when the table is there. A
    /// missing table is probed again on every operation: another process can
    /// create it at any moment, and a memoized "missing" would answer every
    /// read of it as empty until this process happened to invalidate it. The
    /// explicit [`schema_table_exists`](Self::schema_table_exists) API
    /// deliberately bypasses this and stays a live probe for callers that
    /// want ground truth.
    async fn table_present_for_op(&self, table: &str) -> Result<bool, DatabaseError> {
        if self.strict_schema() {
            return Ok(true);
        }
        let cache = self.schema_cache();
        if cache.is_some_and(|c| c.table_known_present(table)) {
            return Ok(true);
        }
        // Snapshot the generation *before* the probe yields; the gen-guarded
        // write-back below is dropped if a mutation raced the probe (see
        // `SchemaCache` docs). Holding `Option<&SchemaCache>` across the await
        // is fine — it is a plain reference, never a lock guard.
        let gen0 = cache.map(SchemaCache::generation);
        let exists = self.dbx_table_exists(table).await?;
        if let (true, Some(cache), Some(gen0)) = (exists, cache, gen0) {
            cache.mark_table_present_if_gen(table, gen0);
        }
        Ok(exists)
    }

    /// Where the `id` of a row created in `table` without one comes from
    /// ([`IdPolicy`], see [`introspect::build_id_policy`]): minted by
    /// [`create`](Self::create), filled by the database (a SQLite rowid
    /// alias, a Postgres identity or sequence-backed column), or required of
    /// the caller (an integer `id` nothing fills).
    ///
    /// Consults [`schema_cache`](Self::schema_cache) first and populates it
    /// on a miss. Runs in STRICT_SCHEMA mode too: nothing else tells the
    /// executor which tables number their own rows. A missing table answers
    /// [`IdPolicy::Mint`], uncached (see [`SchemaCache::set_id_policy_if_gen`]).
    async fn id_policy(&self, table: &str) -> Result<IdPolicy, DatabaseError> {
        let cache = self.schema_cache();
        if let Some(policy) = cache.and_then(|c| c.id_policy(table)) {
            return Ok(policy);
        }
        // Generation snapshot before the probe yields, as in `table_columns`.
        let gen0 = cache.map(SchemaCache::generation);
        let (sql, params) = introspect::build_id_policy(table, Self::BACKEND);
        let code = self.run_scalar_i64(&sql, &params).await?;
        let policy = IdPolicy::from_code(code).ok_or_else(|| {
            DatabaseError::Internal(format!("id policy probe of {table} answered {code}"))
        })?;
        if let (Some(cache), Some(gen0)) = (cache, gen0) {
            cache.set_id_policy_if_gen(table, policy, gen0);
        }
        Ok(policy)
    }

    /// The [`IdPolicy`] that applies to `data`, a row about to be created in
    /// `table`: `None` when it carries its own `id`, so no probe is issued.
    async fn created_row_id_policy(
        &self,
        table: &str,
        data: &HashMap<String, serde_json::Value>,
    ) -> Result<Option<IdPolicy>, DatabaseError> {
        if data.contains_key("id") {
            Ok(None)
        } else {
            self.id_policy(table).await.map(Some)
        }
    }

    /// Column names (lowercased) of `table`; empty if the table is missing.
    ///
    /// Consults [`schema_cache`](Self::schema_cache) first and populates it on
    /// a miss (see [`table_columns`](Self::table_columns)).
    async fn get_columns(&self, table: &str) -> Result<Vec<String>, DatabaseError> {
        Ok(self.table_columns(table).await?.names)
    }

    /// The columns of `table` declared to hold JSON, which every row read from
    /// it is decoded with (see [`codec`](super::codec)); none if the table is
    /// missing.
    ///
    /// Runs in STRICT_SCHEMA mode too: nothing else can tell a backend whose
    /// driver reports no column types which text is JSON. A backend with a
    /// [`schema_cache`](Self::schema_cache) issues the introspection once per
    /// table.
    async fn json_columns(&self, table: &str) -> Result<JsonColumns, DatabaseError> {
        Ok(self.table_columns(table).await?.json)
    }

    /// Columns of `table` — names and the JSON-declared subset — from
    /// [`schema_cache`](Self::schema_cache), populated on a miss, so a warm
    /// backend answers without a round-trip. Shared across backends via the
    /// parameter-bound [`introspect::build_list_columns`] builder, whose result
    /// shape (`name` and `decl_type` per column) is identical in both
    /// dialects.
    async fn table_columns(&self, table: &str) -> Result<TableColumns, DatabaseError> {
        let cache = self.schema_cache();
        if let Some(columns) = cache.and_then(|c| c.columns(table)) {
            return Ok(columns);
        }
        // Snapshot the generation before the probe yields; a mutation racing
        // the introspection drops the write-back rather than caching a stale
        // column set (see `SchemaCache` docs).
        let gen0 = cache.map(SchemaCache::generation);
        let columns = self.introspect_columns(table).await?;
        if let (Some(cache), Some(gen0)) = (cache, gen0) {
            cache.set_columns_if_gen(table, columns.clone(), gen0);
        }
        Ok(columns)
    }

    /// Columns of `table` as the database reports them now, bypassing and not
    /// touching [`schema_cache`](Self::schema_cache); empty if the table is
    /// missing.
    async fn introspect_columns(&self, table: &str) -> Result<TableColumns, DatabaseError> {
        let (sql, params) = introspect::build_list_columns(table, Self::BACKEND);
        let rows = self.run_fetch(&sql, &params, JsonColumns::NONE).await?;
        let mut names = Vec::with_capacity(rows.len());
        let mut json = Vec::new();
        for row in rows {
            let Some(name) = row.data.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let name = name.to_lowercase();
            let decl_type = row
                .data
                .get("decl_type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if introspect::is_json_decl_type(decl_type) {
                json.push(name.clone());
            }
            names.push(name);
        }
        Ok(TableColumns {
            names,
            json: JsonColumns::new(json),
        })
    }

    /// Primary-key columns of `table`, in key order; empty when the table has
    /// no primary key (or does not exist).
    ///
    /// Consults [`schema_cache`](Self::schema_cache) first and populates it on
    /// a miss via [`introspect::build_list_primary_key`], whose result shape
    /// (`name` per key column) is identical in both dialects. Runs in
    /// STRICT_SCHEMA mode too: nothing else can tell the executor which
    /// columns identify a row.
    ///
    /// An empty answer is cached only for a table known to exist. The key
    /// introspection of a missing table is empty too, and in STRICT_SCHEMA
    /// mode nothing else probes existence, so a list against a table a later
    /// migration creates would otherwise pin "no key" for the cache's life.
    /// When the cache does not already know the table exists, an empty key
    /// costs one [`dbx_table_exists`](Self::dbx_table_exists) probe, once per
    /// keyless table.
    async fn get_primary_key(&self, table: &str) -> Result<Vec<String>, DatabaseError> {
        let cache = self.schema_cache();
        if let Some(key) = cache.and_then(|c| c.primary_key(table)) {
            return Ok(key);
        }
        // Generation snapshot before the probe yields, as in `get_columns`.
        let gen0 = cache.map(SchemaCache::generation);
        let (sql, params) = introspect::build_list_primary_key(table, Self::BACKEND);
        let rows = self.run_fetch(&sql, &params, JsonColumns::NONE).await?;
        let key: Vec<String> = rows
            .into_iter()
            .filter_map(|r| {
                r.data
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        if let (Some(cache), Some(gen0)) = (cache, gen0) {
            // The setter drops an empty key unless the entry knows the table
            // exists; settle that first. Only a positive answer is recorded:
            // a table that does not exist yet stays unprobed, so the next
            // lookup asks again.
            if key.is_empty()
                && !cache.table_known_present(table)
                && self.dbx_table_exists(table).await?
            {
                cache.mark_table_present_if_gen(table, gen0);
            }
            cache.set_primary_key_if_gen(table, key.clone(), gen0);
        }
        Ok(key)
    }

    /// Add `column` to `table` via `stmt` unless a concurrent writer beat us
    /// to it.
    ///
    /// SQLite has no `ADD COLUMN IF NOT EXISTS`, so the callers' check-then-add
    /// has a race window between the existence check and the `ALTER`. On
    /// failure, re-check: a now-present column means another writer added it
    /// (benign); a still-missing column is a real DDL error and propagates.
    ///
    /// Either way the `ALTER` (attempted or raced) changed this table's column
    /// set, so the cached list is invalidated before the re-check — the
    /// re-check then re-introspects the true set rather than trusting a stale
    /// entry.
    async fn add_column_checked(
        &self,
        table: &str,
        column: &str,
        stmt: &wafer_sql_utils::Statement,
    ) -> Result<(), DatabaseError> {
        let outcome = self.run_execute(&stmt.sql, &[]).await;
        if let Some(cache) = self.schema_cache() {
            cache.invalidate(table);
        }
        if let Err(e) = outcome {
            if !self
                .get_columns(table)
                .await?
                .contains(&column.to_lowercase())
            {
                return Err(schema_step_error(&format!("add column {column}"), e));
            }
        }
        Ok(())
    }

    /// Lazily add columns for every key in `data` missing from `table`.
    ///
    /// Column types are derived from the value being written
    /// ([`ddl::build_add_column_for_value`]): Postgres picks a native type
    /// (BOOLEAN/BIGINT/DOUBLE PRECISION/JSONB/TEXT), SQLite `JSON` for an
    /// object or array and TEXT for anything else. The
    /// table itself must already exist via the block's migration files — only
    /// columns are added on demand, per the documented lazy column-add design.
    ///
    /// Every key must pass [`sql_name`]; one that does not refuses the whole
    /// write before any column is added, in STRICT_SCHEMA mode too.
    async fn ensure_data_columns(
        &self,
        table: &str,
        data: &HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        for key in data.keys() {
            sql_name(key)?;
        }
        // STRICT_SCHEMA trusts the migrated schema: no introspection, no lazy
        // ALTER. A write referencing an unmigrated column fails loudly, which
        // is the intended contract in strict mode.
        if self.strict_schema() {
            return Ok(());
        }
        let existing = self.get_columns(table).await?;
        // Sorted for deterministic DDL order (HashMap iteration is random).
        let mut keys: Vec<&String> = data.keys().collect();
        keys.sort();
        for key in keys {
            let column = sql_name(key)?;
            if existing.contains(&column.to_lowercase()) {
                continue;
            }
            let stmt = ddl::build_add_column_for_value(table, column, &data[key], Self::BACKEND)?;
            self.add_column_checked(table, column, &stmt).await?;
        }
        Ok(())
    }

    /// Refuse a statement that names a column `table` does not have, before
    /// it runs. A read — or the filter of a write — never adds a column: the
    /// schema grows only through the write path's data columns and the
    /// explicit schema ops. Without this check a filter on an unknown column
    /// would fail in the backend as an opaque internal error; here it is a
    /// [`DatabaseError::InvalidArgument`] that names the column.
    ///
    /// Every name must also pass [`sql_name`]. In STRICT_SCHEMA mode no column
    /// set is read: the migrated schema is trusted, and a statement naming an
    /// unknown column fails in the backend. A table with no columns does not
    /// exist; the check passes and the statement fails in the backend, as it
    /// did before the check.
    ///
    /// A cached column list can predate a column another connection added, so
    /// a name missing from it is looked up once more, uncached
    /// ([`introspect_columns`](Self::introspect_columns)), before the
    /// statement is refused. That look-up leaves the cache alone unless it
    /// finds the cached list stale, so a caller sending unknown columns costs
    /// one introspection per request — what the failing statement would have
    /// cost — and never evicts the entry other requests are served from.
    async fn require_columns(&self, table: &str, columns: &[&str]) -> Result<(), DatabaseError> {
        for column in columns {
            sql_name(column)?;
        }
        if self.strict_schema() || columns.is_empty() {
            return Ok(());
        }
        let first_missing = |existing: &[String]| {
            columns
                .iter()
                .find(|c| !existing.contains(&c.to_lowercase()))
                .copied()
        };
        let known = self.get_columns(table).await?;
        if !known.is_empty() && first_missing(&known).is_none() {
            return Ok(());
        }
        let cache = self.schema_cache();
        let current = match cache {
            Some(cache) => {
                let current = self.introspect_columns(table).await?.names;
                if current != known {
                    cache.invalidate(table);
                }
                current
            }
            // Without a cache `known` was read just now.
            None => known,
        };
        if current.is_empty() {
            return Ok(());
        }
        match first_missing(&current) {
            Some(column) => Err(DatabaseError::InvalidArgument(format!(
                "`{table}` has no column `{column}`"
            ))),
            None => Ok(()),
        }
    }

    /// Shared `get`: the table's JSON columns → select-by-id → single row.
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        let table = sql_name(collection)?;
        let json = self.json_columns(table).await?;
        let stmt = wafer_sql_utils::query::build_select_by_id(table, id, Self::BACKEND);
        self.run_fetch_one(&stmt.sql, &sea_values_to_json(stmt.values), &json)
            .await
    }

    /// Shared `list`: pagination check → table-exists guard →
    /// [`require_columns`](Self::require_columns) → primary key → optional
    /// count → select.
    ///
    /// A zero `limit`, or a positive `offset` with no `limit`, is
    /// [`DatabaseError::InvalidArgument`] on every backend, table present or
    /// not (see [`wafer_sql_utils::query::check_pagination`]).
    ///
    /// A sorted or paged select ends its `ORDER BY` with the table's primary
    /// key ([`get_primary_key`](Self::get_primary_key), passed to the builder
    /// as its `unique_key`), so rows that tie on every sort key come back in
    /// the same order on every query and `limit`/`offset` pages are disjoint
    /// and complete. A table with no primary key orders by `opts.sort` alone.
    ///
    /// `opts.filter_tree`, when `Some`, renders via
    /// [`wafer_sql_utils::query::build_condition_tree`] and is AND-ed onto
    /// the flat `opts.filters` clause as the `extra_condition` of both the
    /// COUNT and the SELECT — the same `Cond` folds into both, so
    /// `total_count` always matches the rows actually returned. A `None`
    /// tree (or an empty one) is a no-op, so legacy callers that only ever
    /// set `opts.filters` are unaffected.
    async fn list(
        &self,
        collection: &str,
        opts: &ListOptions,
    ) -> Result<RecordList, DatabaseError> {
        let table = sql_name(collection)?;
        wafer_sql_utils::query::check_pagination(opts.limit, opts.offset)?;
        if !self.table_present_for_op(table).await? {
            return Ok(RecordList {
                records: Vec::new(),
                total_count: 0,
                page: 1,
                page_size: opts.limit.map_or(0, i64::from),
            });
        }

        self.require_columns(
            table,
            &query_columns(
                &opts.filters,
                &opts.sort,
                opts.filter_tree.as_deref(),
                opts.columns.as_deref(),
            ),
        )
        .await?;

        let primary_key = if wafer_sql_utils::query::orders_rows(opts) {
            self.get_primary_key(table).await?
        } else {
            Vec::new()
        };
        let json = self.json_columns(table).await?;

        // Render both statements to `Statement` (plain `String` + `Vec<Value>`,
        // both `Send`) before any `.await` below, inside a nested block so
        // the intermediate `Cond` is fully dropped (its storage freed) by
        // the closing brace — *before* the generator state machine for this
        // async fn (via `#[wafer_async_trait]` / `async_trait`) crosses an
        // `.await`. `Cond` — and the sea-query builder types it flows
        // through — carries `Rc<dyn Iden>` internally (this crate doesn't
        // enable sea-query's `thread-safe` feature), so it is **not**
        // `Send`; a value of that type still in scope (even if logically
        // moved-out) at an `.await` point makes the whole future non-`Send`,
        // which the shared `DbExec` trait requires for the native
        // (non-wasm-component) build.
        let (count_stmt, select_stmt) = {
            let unique_key: Vec<&str> = primary_key.iter().map(String::as_str).collect();
            let extra_cond = opts
                .filter_tree
                .as_deref()
                .and_then(wafer_sql_utils::query::build_condition_tree);

            let count_stmt = (!opts.skip_count).then(|| {
                wafer_sql_utils::aggregate::build_count_with_condition(
                    table,
                    &opts.filters,
                    extra_cond.clone(),
                    Self::BACKEND,
                )
            });

            let select_stmt = match &opts.columns {
                Some(cols) => {
                    let refs: Vec<&str> = cols.iter().map(String::as_str).collect();
                    wafer_sql_utils::query::build_select_columns(
                        table,
                        &refs,
                        opts,
                        extra_cond,
                        &unique_key,
                        Self::BACKEND,
                    )?
                }
                None => wafer_sql_utils::query::build_select_with_condition(
                    table,
                    opts,
                    extra_cond,
                    &unique_key,
                    Self::BACKEND,
                )?,
            };
            (count_stmt, select_stmt)
        };

        // Execute count + select together. When a count is requested, issue
        // both statements through one `run_batch` so a batching backend (D1)
        // collapses them into a single round-trip; the sequential default runs
        // them one after another, byte-identical to the prior two separate
        // awaits (and, on a transactional batch, count and select now see one
        // consistent snapshot). `skip_count` has no count statement, so it
        // stays a single `run_fetch`. The JSON params are bound to owned
        // (`Send`) locals so nothing `!Send` crosses the `.await`.
        let (total_count, records): (Option<i64>, Vec<Record>) = match count_stmt {
            Some(count_stmt) => {
                let count_params = sea_values_to_json(count_stmt.values);
                let select_params = sea_values_to_json(select_stmt.values);
                let results = self
                    .run_batch(&[
                        BatchOp::ScalarI64 {
                            sql: &count_stmt.sql,
                            params: &count_params,
                        },
                        BatchOp::Rows {
                            sql: &select_stmt.sql,
                            params: &select_params,
                            json: &json,
                        },
                    ])
                    .await?;
                let mut results = results.into_iter();
                let count = match results.next() {
                    Some(BatchResult::ScalarI64(n)) => n,
                    other => return Err(batch_shape_error("list count", other.as_ref())),
                };
                let records = match results.next() {
                    Some(BatchResult::Rows(r)) => r,
                    other => return Err(batch_shape_error("list select", other.as_ref())),
                };
                (Some(count), records)
            }
            None => {
                let records = self
                    .run_fetch(
                        &select_stmt.sql,
                        &sea_values_to_json(select_stmt.values),
                        &json,
                    )
                    .await?;
                (None, records)
            }
        };

        let page = opts
            .limit
            .map_or(1, |limit| (opts.offset / i64::from(limit)) + 1);
        let total_count = total_count.unwrap_or(records.len() as i64);
        Ok(RecordList {
            records,
            total_count,
            page,
            page_size: opts.limit.map_or(total_count, i64::from),
        })
    }

    /// Shared `count`: table-exists guard → [`require_columns`](Self::require_columns)
    /// → COUNT(*).
    async fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError> {
        let table = sql_name(collection)?;
        if !self.table_present_for_op(table).await? {
            return Ok(0);
        }
        self.require_columns(table, &query_columns(filters, &[], None, None))
            .await?;
        let stmt = wafer_sql_utils::aggregate::build_count(table, filters, Self::BACKEND);
        self.run_scalar_i64(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Shared `sum`: table-exists guard → [`require_columns`](Self::require_columns)
    /// on `field` and the filters → SUM(field). A missing table sums to `0`,
    /// as [`count`](Self::count) counts `0` rows in it.
    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        let table = sql_name(collection)?;
        if !self.table_present_for_op(table).await? {
            return Ok(0.0);
        }
        let mut columns = query_columns(filters, &[], None, None);
        columns.push(field);
        self.require_columns(table, &columns).await?;
        let stmt = wafer_sql_utils::aggregate::build_sum(table, field, filters, Self::BACKEND);
        self.run_scalar_f64(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Shared `create`: id/timestamp defaulting → lazy column-add → INSERT.
    ///
    /// A missing `id` is settled by the table's [`IdPolicy`]
    /// ([`id_policy`](Self::id_policy)): a synthesized UUIDv7 string
    /// ([`mint_record_id`]); or, for a table that fills its own, an INSERT
    /// that returns the stored row (`RETURNING *`, on the write path), the id
    /// the database assigned folded into the returned record; or, for an
    /// integer `id` nothing fills, [`DatabaseError::InvalidArgument`].
    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        let table = sql_name(collection)?;
        let mut data = data;

        let policy = self.created_row_id_policy(table, &data).await?;
        prepare_created_row(table, &mut data, policy)?;

        // Ensure any new columns exist. Table creation itself is the block
        // migration's job; a failure here is a real DDL error and propagates
        // rather than letting the INSERT fail with a confusing
        // "no such column".
        self.ensure_data_columns(table, &data).await?;

        let json = self.json_columns(table).await?;
        let pairs = sorted_pairs(&data, &json)?;
        if policy == Some(IdPolicy::Database) {
            let stmt = wafer_sql_utils::query::build_insert_returning(table, &pairs, Self::BACKEND);
            let stored = self
                .run_execute_returning(&stmt.sql, &sea_values_to_json(stmt.values), &json)
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    DatabaseError::Internal(format!("insert into {table} returned no row"))
                })?;
            let id = stored.data.get("id").cloned().ok_or_else(|| {
                DatabaseError::Internal(format!("insert into {table} returned no id"))
            })?;
            data.insert("id".to_string(), id);
            return Ok(Record {
                id: stored.id,
                data,
            });
        }
        let stmt = wafer_sql_utils::query::build_insert(table, &pairs, Self::BACKEND);
        self.run_execute(&stmt.sql, &sea_values_to_json(stmt.values))
            .await?;
        let id = data.get("id").map(codec::record_id).unwrap_or_default();
        Ok(Record { id, data })
    }

    /// Shared `update`: timestamp stamping → lazy column-add → UPDATE-by-id →
    /// re-fetch. 0 rows affected → `NotFound`.
    async fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        let table = sql_name(collection)?;
        let mut data = data;
        stamp_timestamps(&mut data, false);
        self.ensure_data_columns(table, &data).await?;

        let json = self.json_columns(table).await?;
        let pairs = sorted_pairs(&data, &json)?;
        // Batch the UPDATE with the by-id re-fetch so a batching backend (D1)
        // collapses the two round-trips into one. The re-fetch mirrors
        // [`get`](Self::get) exactly — `build_select_by_id` on the same
        // `table`, decoded row-by-row. The `Execute` result stays the
        // authoritative existence check: 0 rows affected → `NotFound`, exactly
        // as the prior explicit affected-count guard, so the (discarded) select
        // rows are only read when the row actually existed. The sequential
        // default runs UPDATE then select one-by-one — observably identical to
        // the prior `run_execute` + `get` (empty select ⇒ `NotFound`, matching
        // `run_fetch_one`).
        let update_stmt =
            wafer_sql_utils::query::build_update_by_id(table, id, &pairs, Self::BACKEND);
        let select_stmt = wafer_sql_utils::query::build_select_by_id(table, id, Self::BACKEND);
        let update_params = sea_values_to_json(update_stmt.values);
        let select_params = sea_values_to_json(select_stmt.values);
        let results = self
            .run_batch(&[
                BatchOp::Execute {
                    sql: &update_stmt.sql,
                    params: &update_params,
                },
                BatchOp::Rows {
                    sql: &select_stmt.sql,
                    params: &select_params,
                    json: &json,
                },
            ])
            .await?;
        let mut results = results.into_iter();
        let affected = match results.next() {
            Some(BatchResult::Execute(n)) => n,
            other => return Err(batch_shape_error("update statement", other.as_ref())),
        };
        if affected == 0 {
            return Err(DatabaseError::NotFound);
        }
        match results.next() {
            Some(BatchResult::Rows(rows)) => rows.into_iter().next().ok_or(DatabaseError::NotFound),
            other => Err(batch_shape_error("update re-fetch", other.as_ref())),
        }
    }

    /// Shared `delete`: delete-by-id; 0 rows → `NotFound`.
    async fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError> {
        let stmt =
            wafer_sql_utils::query::build_delete_by_id(sql_name(collection)?, id, Self::BACKEND);
        let affected = self
            .run_execute(&stmt.sql, &sea_values_to_json(stmt.values))
            .await?;
        if affected == 0 {
            return Err(DatabaseError::NotFound);
        }
        Ok(())
    }

    /// Shared `delete_where`: bulk delete matching `filters`; missing table
    /// is a no-op. See [`delete_where_count`](Self::delete_where_count).
    async fn delete_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<(), DatabaseError> {
        self.delete_where_count(collection, filters).await?;
        Ok(())
    }

    /// Shared `delete_where_count`: table-exists guard →
    /// [`require_columns`](Self::require_columns) → DELETE, returning the
    /// affected-row count (0 for a missing table).
    async fn delete_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        let table = sql_name(collection)?;
        if !self.table_present_for_op(table).await? {
            return Ok(0);
        }
        self.require_columns(table, &query_columns(filters, &[], None, None))
            .await?;
        let stmt = wafer_sql_utils::query::build_delete_where(table, filters, Self::BACKEND);
        self.run_execute(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Shared `take_where`: DELETE ... RETURNING the deleted rows; missing
    /// table → empty. The filter columns must exist
    /// ([`require_columns`](Self::require_columns)).
    ///
    /// `DELETE … RETURNING` has side effects, so it runs through
    /// [`run_execute_returning`](Self::run_execute_returning) (the write path)
    /// rather than [`run_fetch`](Self::run_fetch) (the read path) — on a
    /// backend with dedicated read-only connections, running the DELETE
    /// through `run_fetch` would return the matching rows without deleting
    /// them.
    async fn take_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError> {
        let table = sql_name(collection)?;
        if !self.table_present_for_op(table).await? {
            return Ok(Vec::new());
        }
        self.require_columns(table, &query_columns(filters, &[], None, None))
            .await?;
        let json = self.json_columns(table).await?;
        let stmt =
            wafer_sql_utils::query::build_delete_where_returning(table, filters, Self::BACKEND);
        self.run_execute_returning(&stmt.sql, &sea_values_to_json(stmt.values), &json)
            .await
    }

    /// Shared `update_where`: bulk UPDATE matching `filters`; missing table →
    /// `NotFound`. Lazily adds the SET columns (typed from the data); the
    /// filter columns must exist ([`require_columns`](Self::require_columns)).
    async fn update_where(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        let table = sql_name(collection)?;
        if !self.table_present_for_op(table).await? {
            return Err(DatabaseError::NotFound);
        }
        self.require_columns(table, &query_columns(filters, &[], None, None))
            .await?;
        let mut data = data;
        stamp_timestamps(&mut data, false);
        self.ensure_data_columns(table, &data).await?;
        let json = self.json_columns(table).await?;
        let pairs = sorted_pairs(&data, &json)?;
        let stmt =
            wafer_sql_utils::query::build_update_where(table, &pairs, filters, Self::BACKEND);
        self.run_execute(&stmt.sql, &sea_values_to_json(stmt.values))
            .await?;
        Ok(())
    }

    /// Shared `update_where_count`: table-exists guard →
    /// [`require_columns`](Self::require_columns) on the filters → lazy
    /// data-column add → UPDATE, returning the affected-row count (0 for a
    /// missing table).
    async fn update_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<i64, DatabaseError> {
        let table = sql_name(collection)?;
        if !self.table_present_for_op(table).await? {
            return Ok(0);
        }
        self.require_columns(table, &query_columns(filters, &[], None, None))
            .await?;
        let mut data = data;
        stamp_timestamps(&mut data, false);
        self.ensure_data_columns(table, &data).await?;
        let json = self.json_columns(table).await?;
        let pairs = sorted_pairs(&data, &json)?;
        let stmt =
            wafer_sql_utils::query::build_update_where(table, &pairs, filters, Self::BACKEND);
        self.run_execute(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Shared `increment_field_where`: single-statement atomic
    /// `SET col = col + delta` on matching rows, returning the affected-row
    /// count (0 for a missing table). `col` and the filter columns must exist
    /// ([`require_columns`](Self::require_columns)).
    async fn increment_field_where(
        &self,
        collection: &str,
        col: &str,
        delta: i64,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        let table = sql_name(collection)?;
        if !self.table_present_for_op(table).await? {
            return Ok(0);
        }
        let mut columns = query_columns(filters, &[], None, None);
        columns.push(col);
        self.require_columns(table, &columns).await?;
        let stmt = wafer_sql_utils::query::build_increment_field_where(
            table,
            col,
            delta,
            filters,
            Self::BACKEND,
        );
        self.run_execute(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Shared `upsert`: render a single `INSERT … ON CONFLICT …` via the
    /// backend's dialect and run it, returning rows affected.
    ///
    /// `SetColumns` renders through
    /// [`wafer_sql_utils::upsert::build_upsert`] (empty update list ⇒
    /// `DO NOTHING`). `WindowedCounter` reads its insert values out of the
    /// spec with [`windowed_counter_row`] (`InvalidArgument` for a shape the
    /// statement would not write in full) and renders the atomic
    /// windowed-counter statement, whose `created_fields` are stamped on
    /// INSERT only while `updated_fields` are re-stamped on conflict, all with
    /// the same RFC 3339 instant [`create`](Self::create) stamps.
    ///
    /// Identifiers are validated at the trust boundary (the database handler's
    /// `to_upsert_spec`) before reaching here, and again inside the builders
    /// — a fail-closed guard, since the windowed-counter column names are
    /// interpolated into `CASE`/`SET` expression text.
    async fn upsert(&self, collection: &str, spec: UpsertSpec) -> Result<i64, DatabaseError> {
        let json = self.json_columns(sql_name(collection)?).await?;
        let stmt = Self::upsert_statement(collection, spec, &json)?;
        self.run_execute(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Render the single `INSERT … ON CONFLICT …` statement behind
    /// [`upsert`](Self::upsert) — shared with [`batch`](Self::batch)'s
    /// `Upsert` op, so the two cannot drift. A value for one of `json`'s
    /// columns is written as its JSON text, as [`create`](Self::create) writes
    /// it.
    fn upsert_statement(
        collection: &str,
        mut spec: UpsertSpec,
        json: &JsonColumns,
    ) -> Result<wafer_sql_utils::Statement, DatabaseError> {
        let table = sql_name(collection)?;
        for (column, value) in &mut spec.data {
            if json.contains(column) {
                *value = encode_json_value(value);
            }
        }
        let stmt = match spec.on_conflict {
            UpsertConflict::SetColumns(update_cols) => {
                let named = spec
                    .data
                    .iter()
                    .map(|(column, _)| column)
                    .chain(&spec.conflict_columns)
                    .chain(&update_cols);
                for column in named {
                    sql_name(column)?;
                }
                let conflict: Vec<&str> =
                    spec.conflict_columns.iter().map(String::as_str).collect();
                let update: Vec<&str> = update_cols.iter().map(String::as_str).collect();
                wafer_sql_utils::upsert::build_upsert(
                    table,
                    &spec.data,
                    &conflict,
                    &update,
                    Self::BACKEND,
                )
            }
            UpsertConflict::WindowedCounter {
                count_field,
                window_field,
                now,
                window_cutoff,
                created_fields,
                updated_fields,
            } => {
                let row = windowed_counter_row(&spec.data, &spec.conflict_columns)?;
                let created: Vec<&str> = created_fields.iter().map(String::as_str).collect();
                let updated: Vec<&str> = updated_fields.iter().map(String::as_str).collect();
                wafer_sql_utils::upsert::build_windowed_counter_upsert(
                    table,
                    row.conflict_column,
                    row.id,
                    row.conflict_value,
                    &count_field,
                    &window_field,
                    &created,
                    &updated,
                    &timestamp_now(),
                    now,
                    window_cutoff,
                    Self::BACKEND,
                )?
            }
        };
        Ok(stmt)
    }

    /// Shared `aggregate`: render the validated [`AggregateSpec`] into a
    /// grouped query for this backend's dialect and run it via the same
    /// row-returning primitive `query_raw` uses, returning one [`Record`] per
    /// group.
    ///
    /// The spec is rendered into a `!Send`
    /// [`GroupedQueryConfig`](wafer_sql_utils::aggregate::GroupedQueryConfig)
    /// inside a nested block whose closing brace drops it (and every
    /// `Rc<dyn Iden>` it holds) *before* the `.await` below — the same pattern
    /// [`DbExec::list`] uses so the future stays `Send` for the native build.
    /// Every name in the spec must pass [`sql_name`] (the handler's
    /// `to_aggregate_spec` checks the wire the same way), and every column it
    /// reads must exist ([`require_columns`](Self::require_columns); a sort
    /// key may also name an output alias). A missing table has no groups, as
    /// [`count`](Self::count) counts `0` rows in it.
    async fn aggregate(
        &self,
        collection: &str,
        spec: AggregateSpec,
    ) -> Result<Vec<Record>, DatabaseError> {
        let table = sql_name(collection)?;
        let aliases = spec.aliases();
        for alias in &aliases {
            sql_name(alias)?;
        }
        if !self.table_present_for_op(table).await? {
            return Ok(Vec::new());
        }
        let columns: Vec<&str> = spec
            .read_columns()
            .into_iter()
            .chain(
                spec.sort
                    .iter()
                    .map(|s| s.field.as_str())
                    .filter(|f| !aliases.contains(f)),
            )
            .collect();
        self.require_columns(table, &columns).await?;
        let stmt = {
            let cfg = spec.into_grouped_config(table.to_string());
            wafer_sql_utils::aggregate::build_grouped_query(cfg, Self::BACKEND)?
        };
        // An aggregate row holds computed values and group keys, not stored
        // rows, so its text decodes as text.
        self.run_fetch(
            &stmt.sql,
            &sea_values_to_json(stmt.values),
            JsonColumns::NONE,
        )
        .await
    }

    /// Shared `query_raw`: pass-through to `run_fetch`.
    ///
    /// Raw SQL names no single source table, so it is decoded with
    /// [`JsonColumns::NONE`]: a JSON column's text comes back as text on the
    /// SQLite family (Postgres still returns `json`/`jsonb` structured).
    ///
    /// Read path (see [`run_fetch`](Self::run_fetch)'s contract): `query_raw`
    /// is the admin SQL-explorer's read entry point, so a caller must use
    /// `exec_raw` for a statement with side effects. A write statement passed
    /// here now errors rather than silently no-op-ing, since `run_fetch`
    /// itself propagates a statement-level failure instead of swallowing it —
    /// but it never applies, on any backend with a read-only path.
    async fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        self.run_fetch(query, args, JsonColumns::NONE).await
    }

    /// Shared `exec_raw`: pass-through to `run_execute`.
    ///
    /// This is the runtime DDL escape hatch — a raw statement may be DDL
    /// (`CREATE`/`ALTER`/`DROP TABLE`) whose target table can't be recovered
    /// from the SQL text here. On success the whole [`schema_cache`](Self::schema_cache)
    /// is conservatively cleared so no stale entry outlives a schema change; a
    /// failed statement changed nothing, so the cache is left intact.
    async fn exec_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let affected = self.run_execute(query, args).await?;
        if let Some(cache) = self.schema_cache() {
            cache.clear();
        }
        Ok(affected)
    }

    /// Shared `schema_columns`: [`get_columns`](Self::get_columns) of
    /// `table`, which must pass [`sql_name`].
    async fn schema_columns(&self, table: &str) -> Result<Vec<String>, DatabaseError> {
        self.get_columns(sql_name(table)?).await
    }

    /// Shared `schema_table_exists`: pass-through to `dbx_table_exists`
    /// (the primitive preserves each backend's error text) for a `name` that
    /// passes [`sql_name`].
    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        self.dbx_table_exists(sql_name(name)?).await
    }

    /// Shared `ensure_schema_table`: `CREATE TABLE IF NOT EXISTS` → add every
    /// declared column the table is missing → indexes → foreign-key indexes.
    ///
    /// Every step is fail-loud. In particular the column adds go through
    /// [`add_column_checked`](Self::add_column_checked), which distinguishes the
    /// two failures the check-then-`ALTER` sequence can produce: a column that
    /// is present after the failure was added by a concurrent writer and is
    /// benign, and a column that is still missing is a real DDL error that
    /// propagates. Demoting the latter to a log line reports a successful
    /// migration and then fails every write against that column with "no such
    /// column" instead — which is what SQLite's hand-written version did.
    ///
    /// The whole call mutates the table's shape, so the memoized existence and
    /// column facts are dropped on **both** paths: a failure may have applied
    /// the DDL partway.
    ///
    /// A backend that needs the sequence to hold one connection/lock for its
    /// whole duration (SQLite's single write worker) overrides this; the
    /// override is about atomicity, not about the policy above.
    async fn ensure_schema_table(
        &self,
        table: &super::service::Table,
    ) -> Result<(), DatabaseError> {
        let outcome = self.run_schema_table_ddl(table).await;
        if let Some(cache) = self.schema_cache() {
            cache.invalidate(&table.name);
        }
        outcome
    }

    /// The DDL sequence behind [`ensure_schema_table`](Self::ensure_schema_table),
    /// split out only so the cache invalidation above covers the error path too.
    /// Not a customization point — override `ensure_schema_table` instead.
    async fn run_schema_table_ddl(
        &self,
        table: &super::service::Table,
    ) -> Result<(), DatabaseError> {
        sql_name(&table.name)?;
        for column in &table.columns {
            sql_name(&column.name)?;
        }
        let create = ddl::build_create_table(table, Self::BACKEND)?;
        self.run_execute(&create.sql, &[])
            .await
            .map_err(|e| schema_step_error(&format!("create table {}", table.name), e))?;

        // The table may predate this schema revision, so add whatever declared
        // column it is missing. `get_columns` is re-read rather than cached
        // from before the CREATE — the CREATE is what made the table exist.
        if let Some(cache) = self.schema_cache() {
            cache.invalidate(&table.name);
        }
        let existing = self.get_columns(&table.name).await?;
        for column in &table.columns {
            if existing.contains(&column.name.to_lowercase()) {
                continue;
            }
            let stmt = ddl::build_add_column(&table.name, column, Self::BACKEND)?;
            self.add_column_checked(&table.name, &column.name, &stmt)
                .await?;
        }

        for index in &table.indexes {
            let stmt = ddl::build_create_index(&table.name, index, Self::BACKEND)?;
            self.run_execute(&stmt.sql, &[])
                .await
                .map_err(|e| schema_step_error("create index", e))?;
        }

        let fk_indexes = ddl::build_fk_indexes(table, Self::BACKEND)?;
        for stmt in fk_indexes {
            self.run_execute(&stmt.sql, &[])
                .await
                .map_err(|e| schema_step_error("create FK index", e))?;
        }
        Ok(())
    }

    /// Insert `rows` into `collection` as **one**
    /// [`run_transaction`](Self::run_transaction), applying
    /// [`create`](Self::create)'s per-row policy (a minted `id` when absent,
    /// `created_at`/`updated_at` stamps when absent).
    ///
    /// Returns the number of rows the backend reports as inserted. Rows may
    /// carry different column sets: each gets its own INSERT, and every column
    /// any row names is lazily added (typed from the first non-null value
    /// written to it) before the transaction starts. That schema step is not
    /// part of the transaction, so a failed insert — or a transaction the
    /// [`statement_budget`](Self::statement_budget) refuses — can leave an
    /// added column behind, never a row.
    async fn create_many(
        &self,
        collection: &str,
        rows: Vec<HashMap<String, serde_json::Value>>,
    ) -> Result<i64, DatabaseError> {
        if rows.is_empty() {
            return Ok(0);
        }
        let table = sql_name(collection)?;
        let policy = if rows.iter().any(|row| !row.contains_key("id")) {
            Some(self.id_policy(table).await?)
        } else {
            None
        };

        // One representative value per column across every row, for the lazy
        // column-add's type choice.
        let mut columns: HashMap<String, serde_json::Value> = HashMap::new();
        let mut rows = rows;
        for data in &mut rows {
            prepare_created_row(table, data, policy.filter(|_| !data.contains_key("id")))?;
            for (key, value) in data.iter() {
                match columns.get(key) {
                    Some(seen) if !seen.is_null() || value.is_null() => {}
                    _ => {
                        columns.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        self.ensure_data_columns(table, &columns).await?;
        let json = self.json_columns(table).await?;
        let mut statements: Vec<(String, Vec<serde_json::Value>)> = Vec::with_capacity(rows.len());
        for data in &rows {
            let stmt = wafer_sql_utils::query::build_insert(
                table,
                &sorted_pairs(data, &json)?,
                Self::BACKEND,
            );
            statements.push((stmt.sql, sea_values_to_json(stmt.values)));
        }

        let ops: Vec<TxOp<'_>> = statements
            .iter()
            .map(|(sql, params)| TxOp::Execute { sql, params })
            .collect();
        self.statement_budget()?.admit(ops.len(), "create_many")?;
        let mut inserted = 0;
        for result in self.run_transaction(&ops).await? {
            match result {
                TxResult::Execute(n) => inserted += n,
                other @ TxResult::Returning(_) => {
                    return Err(DatabaseError::Internal(format!(
                        "run_transaction returned {other:?} for a create_many insert"
                    )))
                }
            }
        }
        Ok(inserted)
    }

    /// Shared `batch`: plan every op's statement (the same statement its
    /// single-op method runs, except that `Create` and `Update` return the
    /// stored row via `RETURNING *`), lazily add the data columns the ops
    /// write (an `UpdateWhere`'s or `DeleteWhere`'s filter columns must
    /// already exist, as for [`update_where_count`](Self::update_where_count)
    /// and [`delete_where_count`](Self::delete_where_count)), then run every
    /// statement as ONE
    /// [`run_transaction`](Self::run_transaction).
    ///
    /// An `UpdateWhere` or `DeleteWhere` against a missing table settles as
    /// `UpdatedWhere`/`DeletedWhere { rows_affected: 0 }` without a
    /// statement, exactly as [`update_where_count`](Self::update_where_count)
    /// and [`delete_where_count`](Self::delete_where_count) return 0; every
    /// other op fails on a missing table, as its single op does.
    ///
    /// The lazy column-adds run before the transaction and are not rolled
    /// back with it, nor undone when the
    /// [`statement_budget`](Self::statement_budget) refuses the transaction.
    /// An empty `ops` runs nothing.
    async fn batch(&self, ops: Vec<WriteOp>) -> Result<Vec<WriteOutcome>, DatabaseError> {
        /// How the statement planned for an op decodes back into its outcome,
        /// or the outcome of an op that needs no statement.
        enum Planned {
            Created(JsonColumns),
            Updated(JsonColumns),
            Deleted,
            UpdatedWhere,
            DeletedWhere,
            Upserted,
            Settled(WriteOutcome),
        }

        let mut planned = Vec::with_capacity(ops.len());
        let mut statements: Vec<(String, Vec<serde_json::Value>)> = Vec::with_capacity(ops.len());
        for op in ops {
            let (kind, stmt) = match op {
                WriteOp::Create {
                    collection,
                    mut data,
                } => {
                    let table = sql_name(&collection)?;
                    let policy = self.created_row_id_policy(table, &data).await?;
                    prepare_created_row(table, &mut data, policy)?;
                    self.ensure_data_columns(table, &data).await?;
                    let json = self.json_columns(table).await?;
                    let stmt = wafer_sql_utils::query::build_insert_returning(
                        table,
                        &sorted_pairs(&data, &json)?,
                        Self::BACKEND,
                    );
                    (Planned::Created(json), stmt)
                }
                WriteOp::Update {
                    collection,
                    id,
                    mut data,
                } => {
                    let table = sql_name(&collection)?;
                    stamp_timestamps(&mut data, false);
                    self.ensure_data_columns(table, &data).await?;
                    let json = self.json_columns(table).await?;
                    let stmt = wafer_sql_utils::query::build_update_by_id_returning(
                        table,
                        &id,
                        &sorted_pairs(&data, &json)?,
                        Self::BACKEND,
                    );
                    (Planned::Updated(json), stmt)
                }
                WriteOp::Delete { collection, id } => {
                    let stmt = wafer_sql_utils::query::build_delete_by_id(
                        sql_name(&collection)?,
                        &id,
                        Self::BACKEND,
                    );
                    (Planned::Deleted, stmt)
                }
                WriteOp::UpdateWhere {
                    collection,
                    filters,
                    mut data,
                } => {
                    let table = sql_name(&collection)?;
                    if !self.table_present_for_op(table).await? {
                        planned.push(Planned::Settled(WriteOutcome::UpdatedWhere {
                            rows_affected: 0,
                        }));
                        continue;
                    }
                    self.require_columns(table, &query_columns(&filters, &[], None, None))
                        .await?;
                    stamp_timestamps(&mut data, false);
                    self.ensure_data_columns(table, &data).await?;
                    let json = self.json_columns(table).await?;
                    let stmt = wafer_sql_utils::query::build_update_where(
                        table,
                        &sorted_pairs(&data, &json)?,
                        &filters,
                        Self::BACKEND,
                    );
                    (Planned::UpdatedWhere, stmt)
                }
                WriteOp::DeleteWhere {
                    collection,
                    filters,
                } => {
                    let table = sql_name(&collection)?;
                    if !self.table_present_for_op(table).await? {
                        planned.push(Planned::Settled(WriteOutcome::DeletedWhere {
                            rows_affected: 0,
                        }));
                        continue;
                    }
                    self.require_columns(table, &query_columns(&filters, &[], None, None))
                        .await?;
                    let stmt =
                        wafer_sql_utils::query::build_delete_where(table, &filters, Self::BACKEND);
                    (Planned::DeletedWhere, stmt)
                }
                WriteOp::Upsert { collection, spec } => {
                    let json = self.json_columns(sql_name(&collection)?).await?;
                    (
                        Planned::Upserted,
                        Self::upsert_statement(&collection, spec, &json)?,
                    )
                }
            };
            planned.push(kind);
            statements.push((stmt.sql, sea_values_to_json(stmt.values)));
        }
        if statements.is_empty() {
            return Ok(planned
                .into_iter()
                .filter_map(|kind| match kind {
                    Planned::Settled(outcome) => Some(outcome),
                    _ => None,
                })
                .collect());
        }

        let tx_ops: Vec<TxOp<'_>> = statements
            .iter()
            .zip(planned.iter().filter(|k| !matches!(k, Planned::Settled(_))))
            .map(|((sql, params), kind)| match kind {
                Planned::Created(json) | Planned::Updated(json) => {
                    TxOp::Returning { sql, params, json }
                }
                _ => TxOp::Execute { sql, params },
            })
            .collect();
        self.statement_budget()?.admit(tx_ops.len(), "batch")?;
        let results = self.run_transaction(&tx_ops).await?;
        if results.len() != statements.len() {
            return Err(DatabaseError::Internal(format!(
                "run_transaction returned {} results for {} statements",
                results.len(),
                statements.len()
            )));
        }

        let mut results = results.into_iter();
        planned
            .into_iter()
            .map(|kind| {
                if let Planned::Settled(outcome) = kind {
                    return Ok(outcome);
                }
                let result = results.next().ok_or_else(|| {
                    DatabaseError::Internal("run_transaction returned too few results".into())
                })?;
                match (kind, result) {
                    (Planned::Created(_), TxResult::Returning(rows)) => rows
                        .into_iter()
                        .next()
                        .map(WriteOutcome::Created)
                        .ok_or_else(|| {
                            DatabaseError::Internal("INSERT … RETURNING returned no row".into())
                        }),
                    (Planned::Updated(_), TxResult::Returning(rows)) => {
                        Ok(WriteOutcome::Updated(rows.into_iter().next()))
                    }
                    (Planned::Deleted, TxResult::Execute(rows_affected)) => {
                        Ok(WriteOutcome::Deleted { rows_affected })
                    }
                    (Planned::UpdatedWhere, TxResult::Execute(rows_affected)) => {
                        Ok(WriteOutcome::UpdatedWhere { rows_affected })
                    }
                    (Planned::DeletedWhere, TxResult::Execute(rows_affected)) => {
                        Ok(WriteOutcome::DeletedWhere { rows_affected })
                    }
                    (Planned::Upserted, TxResult::Execute(rows_affected)) => {
                        Ok(WriteOutcome::Upserted { rows_affected })
                    }
                    (_, other) => Err(DatabaseError::Internal(format!(
                        "run_transaction returned {other:?} for a statement of the other kind"
                    ))),
                }
            })
            .collect()
    }

    /// Run one guarded write from [`wafer_sql_utils::guard`] so that its
    /// guard check and its write are one atomic step against every other
    /// guarded write to `table`, as ONE
    /// [`run_transaction`](Self::run_transaction):
    /// [`guard::build_guard_preamble`] (READ COMMITTED and the table's lock,
    /// on PostgreSQL), then — when there are guards — a
    /// [`guard::build_guard_probe`] of every guard's verdict, the write, and
    /// the same probe again.
    ///
    /// Returns the write's own result — its rows, decoded with `returning`'s
    /// JSON columns, when `returning` is `Some`; else its affected count — and, for a write that did nothing, the index of the
    /// guard that refused it. That is the first refusing guard of the probe
    /// before the write or, when that probe passed, of the probe after it:
    /// every guarded write waits on the lock, so between the probes only an
    /// unguarded write can change the table, and one that commits after the
    /// first probe and before the write is seen by the second. `None` means
    /// neither probe saw a guard refuse.
    async fn run_guarded(
        &self,
        table: &str,
        guards: &[CapGuard],
        write: wafer_sql_utils::Statement,
        returning: Option<&JsonColumns>,
    ) -> Result<(TxResult, Option<usize>), DatabaseError> {
        let mut statements: Vec<(String, Vec<serde_json::Value>)> =
            guard::build_guard_preamble(table, Self::BACKEND)
                .into_iter()
                .map(|stmt| (stmt.sql, sea_values_to_json(stmt.values)))
                .collect();
        let preamble = statements.len();
        let probe = if guards.is_empty() {
            None
        } else {
            let probe = guard::build_guard_probe(table, guards, Self::BACKEND)
                .map_err(|e| DatabaseError::Internal(e.to_string()))?;
            Some((probe.sql, sea_values_to_json(probe.values)))
        };
        statements.extend(probe.clone());
        let write_at = statements.len();
        statements.push((write.sql, sea_values_to_json(write.values)));
        statements.extend(probe);

        let ops: Vec<TxOp<'_>> = statements
            .iter()
            .enumerate()
            .map(|(i, (sql, params))| match (i, returning) {
                (i, _) if i < preamble => TxOp::Execute { sql, params },
                (i, Some(json)) if i == write_at => TxOp::Returning { sql, params, json },
                (i, None) if i == write_at => TxOp::Execute { sql, params },
                // A guard probe: one row of computed verdicts.
                _ => TxOp::Returning {
                    sql,
                    params,
                    json: JsonColumns::NONE,
                },
            })
            .collect();
        self.statement_budget()?
            .admit(ops.len(), "a guarded write")?;
        let results = self.run_transaction(&ops).await?;
        if results.len() != ops.len() {
            return Err(DatabaseError::Internal(format!(
                "run_transaction returned {} results for {} statements",
                results.len(),
                ops.len()
            )));
        }
        let mut results = results.into_iter().skip(preamble);
        if guards.is_empty() {
            let write_result = results.next().ok_or_else(|| {
                DatabaseError::Internal("run_transaction returned no write result".into())
            })?;
            return Ok((write_result, None));
        }
        let (Some(before), Some(write_result), Some(after)) =
            (results.next(), results.next(), results.next())
        else {
            return Err(DatabaseError::Internal(
                "run_transaction returned too few results for a guarded write".into(),
            ));
        };
        let refused = match first_refusing_guard(before, guards.len())? {
            Some(index) => Some(index),
            None => first_refusing_guard(after, guards.len())?,
        };
        Ok((write_result, refused))
    }

    /// Shared `insert_guarded`: [`require_columns`](Self::require_columns) on
    /// every column the guards name (a guard never adds one), then
    /// [`create`](Self::create)'s id/timestamp policy and lazy data-column
    /// add, then ONE [`guard::build_insert_guarded`]
    /// statement (`INSERT … SELECT … WHERE {guards} RETURNING *`) through
    /// [`run_guarded`](Self::run_guarded).
    ///
    /// A refused insert names its guard even when an unguarded write
    /// committing between the first probe and the insert is what refused it
    /// (see [`run_guarded`](Self::run_guarded)).
    async fn insert_guarded(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
        guards: &[CapGuard],
    ) -> Result<GuardedInsert, DatabaseError> {
        let table = sql_name(collection)?;
        self.require_columns(table, &guard_columns(guards)).await?;
        let mut data = data;
        let policy = self.created_row_id_policy(table, &data).await?;
        prepare_created_row(table, &mut data, policy)?;
        self.ensure_data_columns(table, &data).await?;
        let json = self.json_columns(table).await?;
        let stmt =
            guard::build_insert_guarded(table, &sorted_pairs(&data, &json)?, guards, Self::BACKEND)
                .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        match self.run_guarded(table, guards, stmt, Some(&json)).await? {
            (TxResult::Returning(rows), refused) => match (rows.into_iter().next(), refused) {
                (Some(row), _) => Ok(GuardedInsert::Inserted(row)),
                (None, Some(guard)) => Ok(GuardedInsert::Refused { guard }),
                (None, None) => Err(DatabaseError::Internal(format!(
                    "guarded insert into {table} wrote nothing, yet every guard held both before \
                     and after it: unguarded writes changed the table and changed it back"
                ))),
            },
            (other @ TxResult::Execute(_), _) => Err(DatabaseError::Internal(format!(
                "run_transaction returned {other:?} for a guarded insert"
            ))),
        }
    }

    /// Shared `update_guarded`: table-exists guard (a missing table matches
    /// nothing) → [`require_columns`](Self::require_columns) on the filters
    /// and guards → timestamp stamping → lazy data-column add →
    /// ONE [`guard::build_update_guarded`] statement through
    /// [`run_guarded`](Self::run_guarded).
    async fn update_guarded(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
        guards: &[CapGuard],
    ) -> Result<GuardedUpdate, DatabaseError> {
        let table = sql_name(collection)?;
        if !self.table_present_for_op(table).await? {
            return Ok(GuardedUpdate::NoMatch);
        }
        let mut columns = query_columns(filters, &[], None, None);
        columns.extend(guard_columns(guards));
        self.require_columns(table, &columns).await?;
        let mut data = data;
        stamp_timestamps(&mut data, false);
        self.ensure_data_columns(table, &data).await?;
        let json = self.json_columns(table).await?;
        let stmt = guard::build_update_guarded(
            table,
            &sorted_pairs(&data, &json)?,
            filters,
            guards,
            Self::BACKEND,
        )
        .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        match self.run_guarded(table, guards, stmt, None).await? {
            (TxResult::Execute(rows_affected), _) if rows_affected > 0 => {
                Ok(GuardedUpdate::Updated { rows_affected })
            }
            (TxResult::Execute(_), Some(guard)) => Ok(GuardedUpdate::Refused { guard }),
            (TxResult::Execute(_), None) => Ok(GuardedUpdate::NoMatch),
            (other @ TxResult::Returning(_), _) => Err(DatabaseError::Internal(format!(
                "run_transaction returned {other:?} for a guarded update"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Notify;

    use super::*;

    /// Minted ids are UUIDv7 and sort in the order they were minted, even
    /// many to a millisecond, so a list that breaks ties on the key lists
    /// same-stamp rows in creation order.
    #[test]
    fn minted_record_ids_are_v7_and_increase_in_mint_order() {
        let ids: Vec<String> = (0..2000).map(|_| mint_record_id()).collect();
        for id in &ids {
            let parsed = uuid::Uuid::parse_str(id).expect("minted id is a UUID");
            assert_eq!(parsed.get_version_num(), 7, "{id} is not a v7 UUID");
        }
        for pair in ids.windows(2) {
            assert!(
                pair[0] < pair[1],
                "{} then {} is out of order",
                pair[0],
                pair[1]
            );
        }
    }

    /// Mock backend whose `dbx_table_exists` parks on a barrier mid-probe, so a
    /// test can fire an invalidation into the exact TOCTOU window between the
    /// probe's generation snapshot and its cache write-back. Every other
    /// primitive is an inert stub.
    struct BarrierExec {
        cache: SchemaCache,
        entered_probe: Arc<Notify>,
        release_probe: Arc<Notify>,
        exists: bool,
    }

    #[wafer_async_trait]
    impl DbExec for BarrierExec {
        const BACKEND: Backend = Backend::Sqlite;

        fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
            Ok(StatementBudget::Unbounded)
        }

        fn schema_cache(&self) -> Option<&SchemaCache> {
            Some(&self.cache)
        }

        async fn run_fetch(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Vec<Record>, DatabaseError> {
            Ok(Vec::new())
        }

        async fn run_fetch_one(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Record, DatabaseError> {
            Err(DatabaseError::NotFound)
        }

        async fn run_execute(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            Ok(0)
        }

        async fn run_execute_returning(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Vec<Record>, DatabaseError> {
            Ok(Vec::new())
        }

        async fn run_scalar_i64(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            Ok(0)
        }

        async fn run_scalar_f64(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<f64, DatabaseError> {
            Ok(0.0)
        }

        async fn run_transaction(&self, _ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, DatabaseError> {
            Err(DatabaseError::Internal(
                "run_transaction is not exercised by this mock".into(),
            ))
        }

        async fn dbx_table_exists(&self, _table: &str) -> Result<bool, DatabaseError> {
            // Announce that we've captured gen0 and are parked in the probe,
            // then wait to be released — the window an invalidation must win.
            self.entered_probe.notify_one();
            self.release_probe.notified().await;
            Ok(self.exists)
        }
    }

    /// Exec-level proof of the linearizability guard: when an invalidation
    /// lands while `table_present_for_op` is parked in its probe, the stale
    /// "present" read (taken just before a concurrent DROP) is NOT written
    /// back — the next op re-probes instead of trusting a resurrected entry.
    #[tokio::test]
    async fn probe_write_back_dropped_when_invalidated_mid_flight() {
        let backend = BarrierExec {
            cache: SchemaCache::new(),
            entered_probe: Arc::new(Notify::new()),
            release_probe: Arc::new(Notify::new()),
            exists: true,
        };
        let entered = backend.entered_probe.clone();
        let release = backend.release_probe.clone();

        let probe = backend.table_present_for_op("orders");
        let racer = async {
            // Wait until the probe has snapshotted gen0 and parked in the DB call.
            entered.notified().await;
            // A concurrent migration DROPs the table and invalidates the cache
            // (bumping the generation past the probe's snapshot).
            backend.cache.invalidate("orders");
            // Release the probe to attempt its now-stale write-back.
            release.notify_one();
        };

        let (present, ()) = tokio::join!(probe, racer);
        // The probe still returns what the DB told it at read time...
        assert!(
            present.expect("probe succeeds"),
            "probe returns its read-time value"
        );
        // ...but that stale value must NOT have been cached.
        assert!(
            !backend.cache.table_known_present("orders"),
            "a probe write-back racing an invalidation must be discarded"
        );
    }

    // -----------------------------------------------------------------------
    // run_batch — sequential default + `list`'s use of it
    // -----------------------------------------------------------------------

    use std::sync::Mutex;

    use wafer_block::db::ListOptions;

    /// Backend that inherits the **default** `run_batch` (no override), so the
    /// tests exercise the sequential fallback. Each primitive records its
    /// dispatch (`"<kind>:<sql>"`) in call order and returns a distinct
    /// sentinel so positional decoding can be checked; `run_execute` fails when
    /// its SQL is `"FAIL"`, to prove first-error-aborts.
    struct SeqMock {
        calls: Mutex<Vec<String>>,
    }

    impl SeqMock {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
        fn record(&self, s: String) {
            self.calls.lock().unwrap().push(s);
        }
        fn record_row(sql: &str) -> Record {
            // id echoes the SQL so a decoded row proves which op produced it.
            Record {
                id: sql.to_string(),
                data: HashMap::new(),
            }
        }
    }

    #[wafer_async_trait]
    impl DbExec for SeqMock {
        const BACKEND: Backend = Backend::Sqlite;

        fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
            Ok(StatementBudget::Unbounded)
        }

        async fn run_fetch(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Vec<Record>, DatabaseError> {
            self.record(format!("fetch:{sql}"));
            Ok(vec![Self::record_row(sql)])
        }

        async fn run_fetch_one(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Record, DatabaseError> {
            self.record(format!("fetch_one:{sql}"));
            Ok(Self::record_row(sql))
        }

        async fn run_execute(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            self.record(format!("execute:{sql}"));
            if sql == "FAIL" {
                return Err(DatabaseError::Internal("boom".into()));
            }
            Ok(7)
        }

        async fn run_execute_returning(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Vec<Record>, DatabaseError> {
            self.record(format!("execute_returning:{sql}"));
            Ok(vec![Self::record_row(sql)])
        }

        async fn run_scalar_i64(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            self.record(format!("scalar_i64:{sql}"));
            Ok(42)
        }

        async fn run_scalar_f64(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<f64, DatabaseError> {
            self.record(format!("scalar_f64:{sql}"));
            Ok(2.5)
        }

        async fn run_transaction(&self, _ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, DatabaseError> {
            Err(DatabaseError::Internal(
                "run_transaction is not exercised by this mock".into(),
            ))
        }

        async fn dbx_table_exists(&self, _table: &str) -> Result<bool, DatabaseError> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn run_batch_default_decodes_each_variant_positionally_and_in_order() {
        let mock = SeqMock::new();
        let results = mock
            .run_batch(&[
                BatchOp::ScalarI64 {
                    sql: "COUNT",
                    params: &[],
                },
                BatchOp::Rows {
                    sql: "SELECT",
                    params: &[],
                    json: JsonColumns::NONE,
                },
                BatchOp::Execute {
                    sql: "UPDATE",
                    params: &[],
                },
                BatchOp::ScalarF64 {
                    sql: "SUM",
                    params: &[],
                },
                BatchOp::FetchOne {
                    sql: "ONE",
                    params: &[],
                    json: JsonColumns::NONE,
                },
            ])
            .await
            .expect("sequential batch succeeds");

        // One result per op, decoded to the variant matching the op, each
        // carrying the value the corresponding single-statement primitive
        // returns.
        assert_eq!(results.len(), 5);
        assert!(matches!(results[0], BatchResult::ScalarI64(42)));
        match &results[1] {
            BatchResult::Rows(rows) => assert_eq!(rows[0].id, "SELECT"),
            other => panic!("expected Rows, got {other:?}"),
        }
        assert!(matches!(results[2], BatchResult::Execute(7)));
        match results[3] {
            BatchResult::ScalarF64(f) => assert!((f - 2.5).abs() < f64::EPSILON),
            ref other => panic!("expected ScalarF64, got {other:?}"),
        }
        match &results[4] {
            BatchResult::FetchOne(r) => assert_eq!(r.id, "ONE"),
            other => panic!("expected FetchOne, got {other:?}"),
        }

        // Primitives were dispatched once each, in submission order.
        let calls = mock.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                "scalar_i64:COUNT",
                "fetch:SELECT",
                "execute:UPDATE",
                "scalar_f64:SUM",
                "fetch_one:ONE",
            ]
        );
    }

    #[tokio::test]
    async fn run_batch_default_aborts_on_first_error_and_skips_later_ops() {
        let mock = SeqMock::new();
        let err = mock
            .run_batch(&[
                BatchOp::Rows {
                    sql: "A",
                    params: &[],
                    json: JsonColumns::NONE,
                },
                BatchOp::Execute {
                    sql: "FAIL",
                    params: &[],
                },
                BatchOp::Rows {
                    sql: "C",
                    params: &[],
                    json: JsonColumns::NONE,
                },
            ])
            .await
            .expect_err("the failing op aborts the batch");
        assert!(matches!(err, DatabaseError::Internal(_)));

        // The op *after* the failure never dispatched — first-error aborts.
        let calls = mock.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec!["fetch:A", "execute:FAIL"],
            "the op after the failing one must not run"
        );
    }

    /// `take_where` builds `DELETE … RETURNING` and must dispatch it through
    /// [`DbExec::run_execute_returning`] (the write path), never
    /// [`DbExec::run_fetch`] (the read path) — on a backend with dedicated
    /// read-only connections, running the DELETE through `run_fetch` returns
    /// the matching rows without deleting them (the bug this primitive
    /// exists to close).
    #[tokio::test]
    async fn take_where_dispatches_through_the_write_path() {
        let mock = SeqMock::new();
        let _ = mock.take_where("t", &[]).await.unwrap();

        let calls = mock.calls.lock().unwrap().clone();
        let delete_call = calls
            .iter()
            .find(|c| c.contains("DELETE") && c.contains("RETURNING"))
            .unwrap_or_else(|| {
                panic!(
                    "no DELETE … RETURNING statement was dispatched at all; calls were: {calls:?}"
                )
            });
        assert!(
            delete_call.starts_with("execute_returning:"),
            "the DELETE … RETURNING statement must be dispatched through \
             run_execute_returning (the write path), not any read-path \
             primitive; got: {delete_call:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("fetch:") && c.contains("DELETE")),
            "the DELETE … RETURNING statement must not be dispatched through \
             run_fetch (the read path); calls were: {calls:?}"
        );
    }

    #[test]
    fn batch_op_sql_params_returns_the_pair_for_every_variant() {
        let params = vec![serde_json::json!("x")];
        for op in [
            BatchOp::Rows {
                sql: "s",
                params: &params,
                json: JsonColumns::NONE,
            },
            BatchOp::FetchOne {
                sql: "s",
                params: &params,
                json: JsonColumns::NONE,
            },
            BatchOp::Execute {
                sql: "s",
                params: &params,
            },
            BatchOp::ScalarI64 {
                sql: "s",
                params: &params,
            },
            BatchOp::ScalarF64 {
                sql: "s",
                params: &params,
            },
        ] {
            let (sql, p) = op.sql_params();
            assert_eq!(sql, "s");
            assert_eq!(p, params.as_slice());
        }
    }

    /// Backend that **overrides** `run_batch` to record the ops it receives and
    /// return canned, positionally-aligned results — so a test can prove
    /// `DbExec::list` hands it exactly `[ScalarI64(count), Rows(select)]`.
    /// `strict_schema` is on so `list` skips the introspection round-trips and
    /// goes straight to the count+select batch. `run_fetch` records itself for
    /// the `skip_count` single-statement path.
    struct BatchMock {
        canned_count: i64,
        batch_calls: Mutex<Vec<Vec<(String, String)>>>,
        tx_calls: Mutex<Vec<Vec<(String, String)>>>,
        fetch_calls: Mutex<Vec<String>>,
        /// `(sql, json)` of every row-returning statement except the column
        /// introspection, whichever primitive carried it.
        row_json: Mutex<Vec<(String, JsonColumns)>>,
        /// The per-invocation statement limit this mock reports, as D1 does,
        /// or `None` for `Unbounded`.
        limit: Option<u64>,
        /// Every statement any primitive ran, each one inside a
        /// `run_transaction` or `run_batch` included, as D1 counts them.
        issued: Mutex<u64>,
    }

    impl BatchMock {
        fn new(canned_count: i64) -> Self {
            Self {
                canned_count,
                batch_calls: Mutex::new(Vec::new()),
                tx_calls: Mutex::new(Vec::new()),
                fetch_calls: Mutex::new(Vec::new()),
                row_json: Mutex::new(Vec::new()),
                limit: None,
                issued: Mutex::new(0),
            }
        }
        /// A mock that runs at most `limit` statements, counting the ones it
        /// issues against it.
        fn limited(limit: u64) -> Self {
            Self {
                limit: Some(limit),
                ..Self::new(0)
            }
        }
        fn issue(&self, statements: usize) {
            *self.issued.lock().unwrap() += statements as u64;
        }
        fn issued(&self) -> u64 {
            *self.issued.lock().unwrap()
        }
        /// The column introspection's answer: `widgets` has one column
        /// declared JSON, spelled in mixed case as a schema may spell it.
        fn declared_columns() -> Vec<Record> {
            [("id", "TEXT"), ("name", "TEXT"), ("Meta", "JSON")]
                .into_iter()
                .map(|(name, decl_type)| Record {
                    id: String::new(),
                    data: HashMap::from([
                        ("name".to_string(), serde_json::json!(name)),
                        ("decl_type".to_string(), serde_json::json!(decl_type)),
                    ]),
                })
                .collect()
        }
        fn json_of(&self, verb: &str) -> Vec<JsonColumns> {
            self.row_json
                .lock()
                .unwrap()
                .iter()
                .filter(|(sql, _)| sql.starts_with(verb))
                .map(|(_, json)| json.clone())
                .collect()
        }
        fn canned_rows() -> Vec<Record> {
            vec![
                Record {
                    id: "r1".into(),
                    data: HashMap::new(),
                },
                Record {
                    id: "r2".into(),
                    data: HashMap::new(),
                },
            ]
        }
    }

    #[wafer_async_trait]
    impl DbExec for BatchMock {
        const BACKEND: Backend = Backend::Sqlite;

        fn strict_schema(&self) -> bool {
            true
        }

        fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
            Ok(match self.limit {
                Some(limit) => StatementBudget::Limited {
                    limit,
                    used: self.issued(),
                },
                None => StatementBudget::Unbounded,
            })
        }

        async fn run_fetch(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
            json: &JsonColumns,
        ) -> Result<Vec<Record>, DatabaseError> {
            self.issue(1);
            if sql.contains("decl_type") {
                return Ok(Self::declared_columns());
            }
            self.fetch_calls.lock().unwrap().push(sql.to_string());
            self.row_json
                .lock()
                .unwrap()
                .push((sql.to_string(), json.clone()));
            Ok(Self::canned_rows())
        }

        async fn run_fetch_one(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
            json: &JsonColumns,
        ) -> Result<Record, DatabaseError> {
            self.issue(1);
            self.row_json
                .lock()
                .unwrap()
                .push((sql.to_string(), json.clone()));
            Ok(Self::canned_rows().swap_remove(0))
        }

        async fn run_execute(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            self.issue(1);
            Ok(0)
        }

        async fn run_execute_returning(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
            json: &JsonColumns,
        ) -> Result<Vec<Record>, DatabaseError> {
            self.issue(1);
            self.row_json
                .lock()
                .unwrap()
                .push((sql.to_string(), json.clone()));
            Ok(Vec::new())
        }

        async fn run_scalar_i64(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            self.issue(1);
            Ok(0)
        }

        async fn run_scalar_f64(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<f64, DatabaseError> {
            self.issue(1);
            Ok(0.0)
        }

        async fn dbx_table_exists(&self, _table: &str) -> Result<bool, DatabaseError> {
            self.issue(1);
            Ok(true)
        }

        async fn run_transaction(&self, ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, DatabaseError> {
            self.issue(ops.len());
            let recorded: Vec<(String, String)> = ops
                .iter()
                .map(|op| {
                    let name = match op {
                        TxOp::Execute { .. } => "Execute",
                        TxOp::Returning { .. } => "Returning",
                    };
                    (name.to_string(), op.sql_params().0.to_string())
                })
                .collect();
            self.tx_calls.lock().unwrap().push(recorded);
            for op in ops {
                if let TxOp::Returning { sql, json, .. } = op {
                    self.row_json
                        .lock()
                        .unwrap()
                        .push(((*sql).to_string(), (*json).clone()));
                }
            }
            Ok(ops
                .iter()
                .map(|op| match op {
                    TxOp::Execute { .. } => TxResult::Execute(1),
                    // A guard probe (the only SELECT sent to a transaction):
                    // its one guard holds.
                    TxOp::Returning { sql, .. } if sql.starts_with("SELECT") => {
                        TxResult::Returning(vec![Record {
                            id: String::new(),
                            data: HashMap::from([("g0".to_string(), serde_json::json!(1))]),
                        }])
                    }
                    TxOp::Returning { .. } => {
                        TxResult::Returning(vec![Self::canned_rows().swap_remove(0)])
                    }
                })
                .collect())
        }

        async fn run_batch(&self, ops: &[BatchOp<'_>]) -> Result<Vec<BatchResult>, DatabaseError> {
            self.issue(ops.len());
            // Record each op as (variant-name, sql) for the assertion.
            let recorded: Vec<(String, String)> = ops
                .iter()
                .map(|op| {
                    let name = match op {
                        BatchOp::Rows { .. } => "Rows",
                        BatchOp::FetchOne { .. } => "FetchOne",
                        BatchOp::Execute { .. } => "Execute",
                        BatchOp::ScalarI64 { .. } => "ScalarI64",
                        BatchOp::ScalarF64 { .. } => "ScalarF64",
                    };
                    let (sql, _) = op.sql_params();
                    (name.to_string(), sql.to_string())
                })
                .collect();
            self.batch_calls.lock().unwrap().push(recorded);
            for op in ops {
                if let BatchOp::Rows { sql, json, .. } | BatchOp::FetchOne { sql, json, .. } = op {
                    self.row_json
                        .lock()
                        .unwrap()
                        .push(((*sql).to_string(), (*json).clone()));
                }
            }

            // Return canned results aligned to each op's variant.
            let out = ops
                .iter()
                .map(|op| match op {
                    BatchOp::ScalarI64 { .. } => BatchResult::ScalarI64(self.canned_count),
                    BatchOp::Rows { .. } => BatchResult::Rows(Self::canned_rows()),
                    BatchOp::Execute { .. } => BatchResult::Execute(1),
                    BatchOp::ScalarF64 { .. } => BatchResult::ScalarF64(0.0),
                    BatchOp::FetchOne { .. } => {
                        BatchResult::FetchOne(Self::canned_rows().swap_remove(0))
                    }
                })
                .collect();
            Ok(out)
        }
    }

    #[tokio::test]
    async fn list_issues_count_and_select_as_one_run_batch() {
        let mock = BatchMock::new(9);
        let list = DbExec::list(&mock, "widgets", &ListOptions::default())
            .await
            .expect("list succeeds via the batching override");

        // Exactly one run_batch call, carrying [ScalarI64(count), Rows(select)]
        // in that order — the count first, the select second.
        let calls = mock.batch_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "list must issue a single batch");
        let ops = &calls[0];
        assert_eq!(ops.len(), 2, "count + select");
        assert_eq!(ops[0].0, "ScalarI64", "first op is the count scalar");
        assert!(
            ops[0].1.contains("COUNT(*)"),
            "count op SQL is a COUNT: {}",
            ops[0].1
        );
        assert_eq!(ops[1].0, "Rows", "second op is the row-returning select");
        assert!(
            ops[1].1.to_uppercase().contains("SELECT"),
            "select op SQL is a SELECT: {}",
            ops[1].1
        );

        // The select is decoded with the table's declared JSON columns.
        assert_eq!(mock.json_of("SELECT"), [JsonColumns::new(["meta"])]);

        // list decoded the batch's ScalarI64 as total_count and Rows as records.
        assert_eq!(list.total_count, 9);
        assert_eq!(list.records.len(), 2);

        // The count path must not touch the single-statement run_fetch.
        assert!(
            mock.fetch_calls.lock().unwrap().is_empty(),
            "count path batches; it must not call run_fetch directly"
        );
    }

    #[tokio::test]
    async fn list_skip_count_uses_single_fetch_not_a_batch() {
        let mock = BatchMock::new(9);
        let opts = ListOptions {
            skip_count: true,
            ..Default::default()
        };
        let list = DbExec::list(&mock, "widgets", &opts)
            .await
            .expect("skip_count list succeeds");

        // No batch — skip_count keeps its single run_fetch.
        assert!(
            mock.batch_calls.lock().unwrap().is_empty(),
            "skip_count must not batch (there is no count statement)"
        );
        let fetches = mock.fetch_calls.lock().unwrap().clone();
        assert_eq!(fetches.len(), 1, "exactly one select");
        assert!(
            fetches[0].to_uppercase().contains("SELECT"),
            "the single statement is the select: {}",
            fetches[0]
        );

        assert_eq!(mock.json_of("SELECT"), [JsonColumns::new(["meta"])]);

        // total_count falls back to records.len() when the count is skipped.
        assert_eq!(list.records.len(), 2);
        assert_eq!(list.total_count, 2);
    }

    // -----------------------------------------------------------------------
    // list ordering: the primary-key tiebreak
    // -----------------------------------------------------------------------

    /// Strict-schema backend with a schema cache whose `run_fetch` records
    /// every statement and answers the primary-key introspection with `key`.
    /// `exists` answers `dbx_table_exists`, and `exists_probes` counts them;
    /// a test flips `exists` and `key` to model a migration landing.
    struct KeyedMock {
        cache: SchemaCache,
        key: Mutex<Vec<&'static str>>,
        exists: Mutex<bool>,
        exists_probes: Mutex<usize>,
        fetches: Mutex<Vec<String>>,
    }

    impl KeyedMock {
        fn new(key: &[&'static str]) -> Self {
            Self {
                cache: SchemaCache::new(),
                key: Mutex::new(key.to_vec()),
                exists: Mutex::new(true),
                exists_probes: Mutex::new(0),
                fetches: Mutex::new(Vec::new()),
            }
        }
        fn fetches(&self) -> Vec<String> {
            self.fetches.lock().unwrap().clone()
        }
    }

    #[wafer_async_trait]
    impl DbExec for KeyedMock {
        const BACKEND: Backend = Backend::Sqlite;

        fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
            Ok(StatementBudget::Unbounded)
        }

        fn schema_cache(&self) -> Option<&SchemaCache> {
            Some(&self.cache)
        }

        fn strict_schema(&self) -> bool {
            true
        }

        async fn run_fetch(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Vec<Record>, DatabaseError> {
            self.fetches.lock().unwrap().push(sql.to_string());
            if sql.contains("decl_type") {
                // The column introspection: a table that exists has a column.
                let exists = *self.exists.lock().unwrap();
                return Ok(exists
                    .then(|| Record {
                        id: String::new(),
                        data: HashMap::from([
                            ("name".to_string(), serde_json::json!("id")),
                            ("decl_type".to_string(), serde_json::json!("TEXT")),
                        ]),
                    })
                    .into_iter()
                    .collect());
            }
            if sql.contains("pk > 0") {
                return Ok(self
                    .key
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|name| Record {
                        id: String::new(),
                        data: HashMap::from([("name".to_string(), serde_json::json!(name))]),
                    })
                    .collect());
            }
            Ok(Vec::new())
        }

        async fn run_fetch_one(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Record, DatabaseError> {
            Err(DatabaseError::NotFound)
        }

        async fn run_execute(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            Ok(0)
        }

        async fn run_execute_returning(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Vec<Record>, DatabaseError> {
            Ok(Vec::new())
        }

        async fn run_scalar_i64(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            Ok(0)
        }

        async fn run_scalar_f64(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<f64, DatabaseError> {
            Ok(0.0)
        }

        async fn run_transaction(&self, _ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, DatabaseError> {
            Err(DatabaseError::Internal(
                "run_transaction is not exercised by this mock".into(),
            ))
        }

        async fn dbx_table_exists(&self, _table: &str) -> Result<bool, DatabaseError> {
            *self.exists_probes.lock().unwrap() += 1;
            Ok(*self.exists.lock().unwrap())
        }
    }

    fn newest_first(limit: Option<u32>, offset: i64) -> ListOptions {
        ListOptions {
            sort: vec![SortField {
                field: "created_at".into(),
                desc: true,
            }],
            limit,
            offset,
            skip_count: true,
            ..Default::default()
        }
    }

    /// A sorted list orders by the introspected primary key last — even in
    /// STRICT_SCHEMA mode — and a warm cache answers the key without another
    /// round-trip.
    #[tokio::test]
    async fn sorted_list_breaks_ties_on_the_introspected_primary_key_once_per_table() {
        let mock = KeyedMock::new(&["token_hash"]);
        for offset in [0, 2] {
            DbExec::list(&mock, "sessions", &newest_first(Some(2), offset))
                .await
                .expect("list");
        }
        let fetches = mock.fetches();
        assert_eq!(
            fetches.len(),
            4,
            "one key probe, one column probe, then two selects: {fetches:?}"
        );
        assert!(
            fetches[0].contains("pk > 0"),
            "key probe first: {}",
            fetches[0]
        );
        assert!(
            fetches[1].contains("decl_type"),
            "then the column probe: {}",
            fetches[1]
        );
        for select in &fetches[2..] {
            assert!(
                select.contains(r#"ORDER BY "created_at" DESC, "token_hash" DESC LIMIT"#),
                "{select}"
            );
        }
    }

    /// An unsorted, unpaged list has no ORDER BY, so it looks nothing up; a
    /// table with no primary key orders by its sort alone.
    #[tokio::test]
    async fn unordered_list_skips_the_key_probe_and_a_keyless_table_sorts_alone() {
        let mock = KeyedMock::new(&["id"]);
        DbExec::list(
            &mock,
            "t",
            &ListOptions {
                skip_count: true,
                ..Default::default()
            },
        )
        .await
        .expect("list");
        let fetches = mock.fetches();
        assert!(
            !fetches.iter().any(|sql| sql.contains("pk > 0")),
            "no key probe: {fetches:?}"
        );
        assert_eq!(
            fetches.last().map(String::as_str),
            Some(r#"SELECT * FROM "t""#)
        );

        let keyless = KeyedMock::new(&[]);
        DbExec::list(&keyless, "t", &newest_first(None, 0))
            .await
            .expect("list");
        let fetches = keyless.fetches();
        assert_eq!(
            fetches.last().map(String::as_str),
            Some(r#"SELECT * FROM "t" ORDER BY "created_at" DESC"#)
        );
    }

    /// In STRICT_SCHEMA mode a list can run before the migration that creates
    /// its table. The key introspection of the missing table is empty, and
    /// caching that would list the table without a tiebreak for the life of
    /// the cache once the migration lands. A keyless table that does exist is
    /// cached after one existence probe.
    #[tokio::test]
    async fn an_empty_key_is_cached_only_for_a_table_that_exists() {
        let mock = KeyedMock::new(&[]);
        *mock.exists.lock().unwrap() = false;
        DbExec::list(&mock, "later", &newest_first(Some(2), 0))
            .await
            .expect("list before the migration");
        assert_eq!(mock.cache.primary_key("later"), None, "not cached");

        // The migration lands out of band: the table now exists with a key.
        *mock.exists.lock().unwrap() = true;
        *mock.key.lock().unwrap() = vec!["id"];
        DbExec::list(&mock, "later", &newest_first(Some(2), 0))
            .await
            .expect("list after the migration");
        let select = mock.fetches().pop().expect("a select");
        assert!(
            select.contains(r#"ORDER BY "created_at" DESC, "id" DESC LIMIT"#),
            "the new key breaks ties: {select}"
        );

        let keyless = KeyedMock::new(&[]);
        for _ in 0..2 {
            DbExec::list(&keyless, "keyless", &newest_first(Some(2), 0))
                .await
                .expect("list keyless");
        }
        assert_eq!(keyless.cache.primary_key("keyless"), Some(Vec::new()));
        assert_eq!(*keyless.exists_probes.lock().unwrap(), 1, "probed once");
        let key_probes = keyless
            .fetches()
            .iter()
            .filter(|sql| sql.contains("pk > 0"))
            .count();
        assert_eq!(key_probes, 1, "the empty key is served from the cache");
    }

    /// A page no backend can render is refused even when the table is
    /// missing and no select would run, so the answer does not depend on
    /// whether a migration has landed yet.
    #[tokio::test]
    async fn an_unrenderable_page_is_refused_before_the_table_probe() {
        let mock = KeyedMock::new(&["id"]);
        *mock.exists.lock().unwrap() = false;
        for (limit, offset) in [(Some(0), 0), (None, 1)] {
            let err = DbExec::list(&mock, "later", &newest_first(limit, offset))
                .await
                .expect_err("refused");
            assert!(
                matches!(err, DatabaseError::InvalidArgument(_)),
                "limit {limit:?} offset {offset}: {err:?}"
            );
        }
        assert_eq!(*mock.exists_probes.lock().unwrap(), 0, "no probe ran");
    }

    // -----------------------------------------------------------------------
    // ensure_schema_table / create_many
    // -----------------------------------------------------------------------

    /// What a mock backend does when it is handed an `ALTER TABLE … ADD COLUMN`.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum AddColumn {
        /// The ALTER succeeds and the column appears.
        Succeeds,
        /// The ALTER fails and the column is still missing — a real DDL error.
        Fails,
        /// The ALTER fails because a concurrent writer already added the
        /// column, so it is present afterwards — a benign lost race.
        FailsButRaced,
    }

    /// Backend that models a table's column set across DDL statements, so the
    /// shared `ensure_schema_table` can be driven through both the real-error
    /// and lost-race paths. `run_fetch` answers only the column-list
    /// introspection query (the sole `run_fetch` caller on this path).
    struct DdlMock {
        cache: SchemaCache,
        columns: Mutex<Vec<String>>,
        executed: Mutex<Vec<String>>,
        add_column: AddColumn,
    }

    impl DdlMock {
        fn new(existing: &[&str], add_column: AddColumn) -> Self {
            Self {
                cache: SchemaCache::new(),
                columns: Mutex::new(existing.iter().map(|c| (*c).to_string()).collect()),
                executed: Mutex::new(Vec::new()),
                add_column,
            }
        }

        fn executed(&self) -> Vec<String> {
            self.executed.lock().unwrap().clone()
        }
    }

    #[wafer_async_trait]
    impl DbExec for DdlMock {
        const BACKEND: Backend = Backend::Sqlite;

        fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
            Ok(StatementBudget::Unbounded)
        }

        fn schema_cache(&self) -> Option<&SchemaCache> {
            Some(&self.cache)
        }

        async fn run_fetch(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Vec<Record>, DatabaseError> {
            let columns = self.columns.lock().unwrap().clone();
            Ok(columns
                .into_iter()
                .map(|name| Record {
                    id: String::new(),
                    data: HashMap::from([("name".to_string(), serde_json::json!(name))]),
                })
                .collect())
        }

        async fn run_fetch_one(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Record, DatabaseError> {
            Err(DatabaseError::NotFound)
        }

        async fn run_execute(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            self.executed.lock().unwrap().push(sql.to_string());
            if !sql.contains("ADD COLUMN") {
                return Ok(0);
            }
            // The declared column name is the quoted ident after ADD COLUMN.
            let added = sql
                .split("ADD COLUMN")
                .nth(1)
                .and_then(|rest| rest.split('"').nth(1))
                .unwrap_or_default()
                .to_string();
            match self.add_column {
                AddColumn::Succeeds => {
                    self.columns.lock().unwrap().push(added);
                    Ok(0)
                }
                AddColumn::Fails => Err(DatabaseError::Internal("alter refused".into())),
                AddColumn::FailsButRaced => {
                    self.columns.lock().unwrap().push(added);
                    Err(DatabaseError::Internal("duplicate column name".into()))
                }
            }
        }

        async fn run_execute_returning(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Vec<Record>, DatabaseError> {
            Ok(Vec::new())
        }

        async fn run_scalar_i64(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            Ok(0)
        }

        async fn run_scalar_f64(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<f64, DatabaseError> {
            Ok(0.0)
        }

        async fn run_transaction(&self, _ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, DatabaseError> {
            Err(DatabaseError::Internal(
                "run_transaction is not exercised by this mock".into(),
            ))
        }

        async fn dbx_table_exists(&self, _table: &str) -> Result<bool, DatabaseError> {
            Ok(true)
        }
    }

    fn ddl_table() -> crate::interfaces::database::service::Table {
        use crate::interfaces::database::service::{col_text, pk, Table};
        Table {
            name: "widgets".to_string(),
            columns: vec![pk("id"), col_text("payload").null()],
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        }
    }

    #[tokio::test]
    async fn ensure_schema_table_creates_then_adds_only_the_missing_columns() {
        let mock = DdlMock::new(&["id"], AddColumn::Succeeds);
        DbExec::ensure_schema_table(&mock, &ddl_table())
            .await
            .expect("ensure_schema_table succeeds");

        let executed = mock.executed();
        assert!(
            executed[0].contains("CREATE TABLE"),
            "the create comes first: {executed:?}"
        );
        let alters: Vec<&String> = executed
            .iter()
            .filter(|s| s.contains("ADD COLUMN"))
            .collect();
        assert_eq!(
            alters.len(),
            1,
            "only the missing column is added: {alters:?}"
        );
        assert!(alters[0].contains("payload"), "{}", alters[0]);
    }

    /// The defect this default closes: SQLite's hand-written version demoted a
    /// failed `ADD COLUMN` to a `warn!` and returned `Ok(())`, so a migration
    /// that could not add a declared column reported success and every later
    /// write failed with "no such column" instead.
    #[tokio::test]
    async fn ensure_schema_table_propagates_a_real_add_column_failure() {
        let mock = DdlMock::new(&["id"], AddColumn::Fails);
        let err = DbExec::ensure_schema_table(&mock, &ddl_table())
            .await
            .expect_err("a failed ADD COLUMN must not be swallowed");
        assert!(
            format!("{err}").contains("payload"),
            "the error names the column: {err}"
        );
    }

    /// A concurrent writer that added the column first is benign: the column is
    /// there, which is all the caller asked for.
    #[tokio::test]
    async fn ensure_schema_table_tolerates_a_lost_add_column_race() {
        let mock = DdlMock::new(&["id"], AddColumn::FailsButRaced);
        DbExec::ensure_schema_table(&mock, &ddl_table())
            .await
            .expect("a lost ADD COLUMN race is not an error");
    }

    /// `ensure_schema_table` mutates the table's shape, so any memoized
    /// existence/column facts must be dropped — including on the failure path,
    /// where the DDL may have applied partway.
    #[tokio::test]
    async fn ensure_schema_table_invalidates_the_schema_cache_even_when_it_fails() {
        let mock = DdlMock::new(&["id"], AddColumn::Fails);
        mock.cache.set_columns_if_gen(
            "widgets",
            TableColumns {
                names: vec!["id".into()],
                json: JsonColumns::NONE.clone(),
            },
            mock.cache.generation(),
        );
        let _ = DbExec::ensure_schema_table(&mock, &ddl_table()).await;
        assert_eq!(
            mock.cache.columns("widgets"),
            None,
            "the stale column list must be gone"
        );
    }

    /// `create_many` must reach the backend as ONE `run_transaction`: that is
    /// what makes it all-or-nothing, and on a backend with a native atomic
    /// multi-statement API it is also one round trip.
    #[tokio::test]
    async fn create_many_issues_exactly_one_transaction_of_inserts() {
        let mock = BatchMock::new(0);
        let rows = vec![
            HashMap::from([("name".to_string(), serde_json::json!("a"))]),
            HashMap::from([("name".to_string(), serde_json::json!("b"))]),
        ];
        let n = DbExec::create_many(&mock, "widgets", rows)
            .await
            .expect("create_many succeeds");
        assert_eq!(n, 2);

        assert!(
            mock.batch_calls.lock().unwrap().is_empty(),
            "the inserts must not go through the non-atomic run_batch"
        );
        let calls = mock.tx_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "one transaction, not one per row");
        assert_eq!(calls[0].len(), 2, "one statement per row");
        for (kind, sql) in &calls[0] {
            assert_eq!(kind, "Execute");
            assert!(sql.to_uppercase().contains("INSERT"), "{sql}");
        }
    }

    fn two_rows() -> Vec<HashMap<String, serde_json::Value>> {
        vec![
            HashMap::from([("name".to_string(), serde_json::json!("a"))]),
            HashMap::from([("name".to_string(), serde_json::json!("b"))]),
        ]
    }

    fn two_creates() -> Vec<WriteOp> {
        two_rows()
            .into_iter()
            .map(|data| WriteOp::Create {
                collection: "widgets".into(),
                data,
            })
            .collect()
    }

    /// The statements a `write` issues on an unlimited mock: its planning
    /// introspection plus its transaction.
    async fn statements_issued<F, Fut>(write: F) -> u64
    where
        F: FnOnce(BatchMock) -> Fut,
        Fut: std::future::Future<Output = BatchMock>,
    {
        write(BatchMock::new(0)).await.issued()
    }

    /// A backend that counts the statements it runs (as D1 does) refuses a
    /// `create_many` whose transaction would overflow what the invocation has
    /// left, AFTER counting the introspection the write ran while planning,
    /// and BEFORE the transaction's first statement. Given exactly enough, the
    /// same write runs.
    #[tokio::test]
    async fn create_many_is_refused_when_its_transaction_overflows_the_remaining_budget() {
        let needed = statements_issued(|mock| async move {
            DbExec::create_many(&mock, "widgets", two_rows())
                .await
                .expect("create_many succeeds unlimited");
            mock
        })
        .await;
        assert!(
            needed > 2,
            "planning introspects before the two INSERTs, so it must be counted too: {needed}"
        );

        let short = BatchMock::limited(needed - 1);
        let err = DbExec::create_many(&short, "widgets", two_rows())
            .await
            .expect_err("one statement short of the budget");
        assert!(
            matches!(err, DatabaseError::ResourceExhausted(_)),
            "fits the limit, not what is left: {err:?}"
        );
        assert!(
            short.tx_calls.lock().unwrap().is_empty(),
            "refused before the transaction ran"
        );

        let exact = BatchMock::limited(needed);
        DbExec::create_many(&exact, "widgets", two_rows())
            .await
            .expect("exactly enough budget");
        assert_eq!(exact.issued(), needed);
    }

    /// As for `create_many`: `batch` is admitted against what is left once
    /// its planning has run, and refused before its transaction.
    #[tokio::test]
    async fn batch_is_refused_when_its_transaction_overflows_the_remaining_budget() {
        let needed = statements_issued(|mock| async move {
            DbExec::batch(&mock, two_creates())
                .await
                .expect("batch succeeds unlimited");
            mock
        })
        .await;
        assert!(needed > 2, "planning is counted: {needed}");

        let short = BatchMock::limited(needed - 1);
        let err = DbExec::batch(&short, two_creates())
            .await
            .expect_err("one statement short of the budget");
        assert!(
            matches!(err, DatabaseError::ResourceExhausted(_)),
            "{err:?}"
        );
        assert!(short.tx_calls.lock().unwrap().is_empty());

        let exact = BatchMock::limited(needed);
        DbExec::batch(&exact, two_creates())
            .await
            .expect("exactly enough budget");
    }

    /// A write that needs more statements than the backend's whole
    /// per-invocation limit can never run, so it is the caller's mistake
    /// (`InvalidArgument`), not a spent invocation (`ResourceExhausted`).
    #[test]
    fn a_write_over_the_whole_limit_is_invalid_and_one_over_the_rest_is_exhausted() {
        let fresh = StatementBudget::Limited { limit: 50, used: 0 };
        assert!(fresh.admit(50, "batch").is_ok());
        assert!(matches!(
            fresh.admit(51, "batch"),
            Err(DatabaseError::InvalidArgument(_))
        ));
        let spent = StatementBudget::Limited {
            limit: 50,
            used: 10,
        };
        assert!(spent.admit(40, "batch").is_ok());
        let Err(DatabaseError::ResourceExhausted(msg)) = spent.admit(41, "batch") else {
            panic!("41 of the 40 left must be ResourceExhausted");
        };
        assert!(
            msg.contains("41") && msg.contains("40") && msg.contains("50"),
            "the refusal names what was asked, what is left and the limit: {msg}"
        );
        assert!(StatementBudget::Unbounded
            .admit(usize::MAX, "batch")
            .is_ok());
    }

    #[tokio::test]
    async fn create_many_stamps_an_id_and_timestamps_on_every_row() {
        let mock = BatchMock::new(0);
        let rows = vec![HashMap::from([(
            "name".to_string(),
            serde_json::json!("a"),
        )])];
        DbExec::create_many(&mock, "widgets", rows)
            .await
            .expect("create_many succeeds");
        let calls = mock.tx_calls.lock().unwrap().clone();
        let sql = &calls[0][0].1;
        for column in ["id", "created_at", "updated_at", "name"] {
            assert!(sql.contains(column), "{column} missing from {sql}");
        }
    }

    /// Each row gets its own INSERT, so rows naming different columns each
    /// insert exactly the columns they carry.
    #[tokio::test]
    async fn create_many_inserts_rows_with_different_column_sets() {
        let mock = BatchMock::new(0);
        let rows = vec![
            HashMap::from([("name".to_string(), serde_json::json!("a"))]),
            HashMap::from([("other".to_string(), serde_json::json!("b"))]),
        ];
        let n = DbExec::create_many(&mock, "widgets", rows)
            .await
            .expect("sparse rows insert");
        assert_eq!(n, 2);
        let calls = mock.tx_calls.lock().unwrap().clone();
        let (first, second) = (&calls[0][0].1, &calls[0][1].1);
        assert!(
            first.contains("\"name\"") && !first.contains("\"other\""),
            "{first}"
        );
        assert!(
            second.contains("\"other\"") && !second.contains("\"name\""),
            "{second}"
        );
    }

    /// `batch` plans every op into ONE `run_transaction`, in order, with the
    /// row-returning form for `Create`/`Update`, and maps each result back to
    /// its op's outcome.
    #[tokio::test]
    async fn batch_runs_every_op_as_one_transaction_in_order() {
        let mock = BatchMock::new(0);
        let ops = vec![
            WriteOp::Create {
                collection: "widgets".into(),
                data: HashMap::from([("name".to_string(), serde_json::json!("a"))]),
            },
            WriteOp::Update {
                collection: "widgets".into(),
                id: "r1".into(),
                data: HashMap::from([("name".to_string(), serde_json::json!("b"))]),
            },
            WriteOp::Delete {
                collection: "widgets".into(),
                id: "r2".into(),
            },
            WriteOp::UpdateWhere {
                collection: "widgets".into(),
                filters: vec![Filter {
                    field: "name".into(),
                    operator: wafer_block::db::FilterOp::Equal,
                    value: serde_json::json!("b"),
                }],
                data: HashMap::from([("name".to_string(), serde_json::json!("c"))]),
            },
            WriteOp::Upsert {
                collection: "widgets".into(),
                spec: UpsertSpec {
                    data: vec![("id".into(), serde_json::json!("r3"))],
                    conflict_columns: vec!["id".into()],
                    on_conflict: UpsertConflict::SetColumns(Vec::new()),
                },
            },
        ];
        let outcomes = DbExec::batch(&mock, ops).await.expect("batch succeeds");

        let calls = mock.tx_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "one transaction for the whole batch");
        let kinds: Vec<&str> = calls[0].iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            ["Returning", "Returning", "Execute", "Execute", "Execute"]
        );
        let verbs: Vec<&str> = calls[0]
            .iter()
            .map(|(_, sql)| sql.split_whitespace().next().unwrap_or(""))
            .collect();
        assert_eq!(verbs, ["INSERT", "UPDATE", "DELETE", "UPDATE", "INSERT"]);
        // The rows a create or update returns decode by the table's JSON columns.
        let meta = JsonColumns::new(["meta"]);
        assert_eq!(mock.json_of("INSERT"), std::slice::from_ref(&meta));
        assert_eq!(mock.json_of("UPDATE"), [meta]);

        assert!(matches!(&outcomes[0], WriteOutcome::Created(r) if r.id == "r1"));
        assert!(matches!(&outcomes[1], WriteOutcome::Updated(Some(r)) if r.id == "r1"));
        assert!(matches!(
            outcomes[2],
            WriteOutcome::Deleted { rows_affected: 1 }
        ));
        assert!(matches!(
            outcomes[3],
            WriteOutcome::UpdatedWhere { rows_affected: 1 }
        ));
        assert!(matches!(
            outcomes[4],
            WriteOutcome::Upserted { rows_affected: 1 }
        ));
    }

    /// A guarded write reaches the backend as ONE `run_transaction` holding
    /// the guard probe and the one conditional write (SQLite needs no lock
    /// statement ahead of them): the guard and the write cannot be split
    /// across calls.
    #[tokio::test]
    async fn guarded_writes_run_one_conditional_statement_in_one_transaction() {
        let mock = BatchMock::new(0);
        let guards = [CapGuard::CountBelow {
            filters: Vec::new(),
            cap: 3,
        }];
        let inserted = DbExec::insert_guarded(
            &mock,
            "widgets",
            HashMap::from([("name".to_string(), serde_json::json!("a"))]),
            &guards,
        )
        .await
        .expect("insert_guarded");
        assert!(matches!(inserted, GuardedInsert::Inserted(_)));
        let updated = DbExec::update_guarded(
            &mock,
            "widgets",
            &[],
            HashMap::from([("name".to_string(), serde_json::json!("b"))]),
            &guards,
        )
        .await
        .expect("update_guarded");
        assert!(matches!(
            updated,
            GuardedUpdate::Updated { rows_affected: 1 }
        ));

        let calls = mock.tx_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "one transaction per guarded write");
        for (call, (kind, verb)) in calls
            .iter()
            .zip([("Returning", "INSERT"), ("Execute", "UPDATE")])
        {
            assert_eq!(call.len(), 3, "probe, write, probe: {call:?}");
            for probe in [&call[0], &call[2]] {
                assert_eq!(probe.0, "Returning");
                assert!(probe.1.starts_with("SELECT (CASE WHEN"), "{}", probe.1);
            }
            assert_eq!(call[1].0, kind);
            assert!(
                call[1].1.starts_with(verb) && call[1].1.contains("SELECT COUNT(*)"),
                "{}",
                call[1].1
            );
        }
        assert!(mock.batch_calls.lock().unwrap().is_empty());
        // The inserted row decodes by the table's JSON columns; a probe's
        // computed verdicts are not stored rows.
        assert_eq!(mock.json_of("INSERT"), [JsonColumns::new(["meta"])]);
        let probes = mock.json_of("SELECT (CASE WHEN");
        assert_eq!(probes.len(), 4, "two probes per guarded write");
        assert!(probes.iter().all(JsonColumns::is_empty), "{probes:?}");
    }

    /// A guarded write is one transaction too (lock preamble, probe, write,
    /// probe), admitted against what the invocation has left after its
    /// planning ran: one statement short, `insert_guarded` and
    /// `update_guarded` are `ResourceExhausted` and send no transaction;
    /// given exactly enough, each runs.
    #[tokio::test]
    async fn guarded_writes_are_refused_when_their_transaction_overflows_the_remaining_budget() {
        let guards: &[CapGuard] = &[CapGuard::CountBelow {
            filters: Vec::new(),
            cap: 3,
        }];
        let row = || HashMap::from([("name".to_string(), serde_json::json!("a"))]);
        let insert = |mock: BatchMock| async move {
            let outcome = DbExec::insert_guarded(&mock, "widgets", row(), guards).await;
            (mock, outcome.map(|_| ()))
        };
        let update = |mock: BatchMock| async move {
            let outcome = DbExec::update_guarded(&mock, "widgets", &[], row(), guards).await;
            (mock, outcome.map(|_| ()))
        };

        let (mock, outcome) = insert(BatchMock::new(0)).await;
        outcome.expect("insert_guarded succeeds unlimited");
        let needed = mock.issued();
        let (short, outcome) = insert(BatchMock::limited(needed - 1)).await;
        assert!(
            matches!(outcome, Err(DatabaseError::ResourceExhausted(_))),
            "insert_guarded one short: {outcome:?}"
        );
        assert!(short.tx_calls.lock().unwrap().is_empty());
        let (_, outcome) = insert(BatchMock::limited(needed)).await;
        outcome.expect("insert_guarded with exactly enough");

        let (mock, outcome) = update(BatchMock::new(0)).await;
        outcome.expect("update_guarded succeeds unlimited");
        let needed = mock.issued();
        let (short, outcome) = update(BatchMock::limited(needed - 1)).await;
        assert!(
            matches!(outcome, Err(DatabaseError::ResourceExhausted(_))),
            "update_guarded one short: {outcome:?}"
        );
        assert!(short.tx_calls.lock().unwrap().is_empty());
        let (_, outcome) = update(BatchMock::limited(needed)).await;
        outcome.expect("update_guarded with exactly enough");
    }

    /// Every read of stored rows is decoded with the JSON columns the table
    /// declares; raw SQL and aggregate rows, which have no single source
    /// table, are decoded with none. A read that dropped the table's JSON
    /// columns would hand a JSON column back as its text, and one that used
    /// them for raw SQL would decode by a table the statement may not read.
    #[tokio::test]
    async fn stored_row_reads_decode_by_the_tables_json_columns() {
        let mock = BatchMock::new(0);
        DbExec::get(&mock, "widgets", "r1").await.expect("get");
        DbExec::take_where(&mock, "widgets", &[])
            .await
            .expect("take_where");
        DbExec::query_raw(&mock, "SELECT 1", &[])
            .await
            .expect("query_raw");
        DbExec::aggregate(
            &mock,
            "widgets",
            AggregateSpec {
                select_columns: Vec::new(),
                aggregates: vec![
                    crate::interfaces::database::service::AggregateColumnSpec::Count {
                        alias: "n".into(),
                    },
                ],
                filters: Vec::new(),
                group_by: Vec::new(),
                sort: Vec::new(),
                limit: 0,
            },
        )
        .await
        .expect("aggregate");

        let meta = JsonColumns::new(["meta"]);
        let recorded = mock.row_json.lock().unwrap().clone();
        let json: Vec<(&str, &JsonColumns)> = recorded
            .iter()
            .map(|(sql, json)| (sql.split_whitespace().next().unwrap_or(""), json))
            .collect();
        assert_eq!(json.len(), 4, "{recorded:?}");
        assert_eq!(json[0], ("SELECT", &meta), "get");
        assert_eq!(json[1], ("DELETE", &meta), "take_where");
        assert_eq!(json[2], ("SELECT", JsonColumns::NONE), "query_raw");
        assert_eq!(json[3], ("SELECT", JsonColumns::NONE), "aggregate");
    }

    /// A backend whose guarded transaction runs a scripted race: the probe
    /// before the write sees guard 0 hold, the write itself changes nothing
    /// (an unguarded write committed in between and tipped the cap), and the
    /// probe after it sees guard 0 refuse. A live server cannot be made to
    /// run another session's commit between two statements of one guarded
    /// transaction on demand, so the interleaving is scripted here.
    struct RacedGuardMock;

    #[wafer_async_trait]
    impl DbExec for RacedGuardMock {
        const BACKEND: Backend = Backend::Sqlite;

        fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
            Ok(StatementBudget::Unbounded)
        }

        fn strict_schema(&self) -> bool {
            true
        }

        async fn run_fetch(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Vec<Record>, DatabaseError> {
            Ok(Vec::new())
        }

        async fn run_fetch_one(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Record, DatabaseError> {
            Err(DatabaseError::NotFound)
        }

        async fn run_execute(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            Ok(0)
        }

        async fn run_execute_returning(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
            _json: &JsonColumns,
        ) -> Result<Vec<Record>, DatabaseError> {
            Ok(Vec::new())
        }

        async fn run_scalar_i64(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            Ok(0)
        }

        async fn run_scalar_f64(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<f64, DatabaseError> {
            Ok(0.0)
        }

        async fn dbx_table_exists(&self, _table: &str) -> Result<bool, DatabaseError> {
            Ok(true)
        }

        async fn run_transaction(&self, ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, DatabaseError> {
            let verdict = |holds: i64| {
                TxResult::Returning(vec![Record {
                    id: String::new(),
                    data: HashMap::from([("g0".to_string(), serde_json::json!(holds))]),
                }])
            };
            let mut probes = [1, 0].into_iter();
            Ok(ops
                .iter()
                .map(|op| match op {
                    TxOp::Returning { sql, .. } if sql.starts_with("SELECT") => {
                        verdict(probes.next().expect("two probes"))
                    }
                    TxOp::Returning { .. } => TxResult::Returning(Vec::new()),
                    TxOp::Execute { .. } => TxResult::Execute(0),
                })
                .collect())
        }
    }

    /// A write refused by an unguarded write that slipped in after the first
    /// probe is still reported as refused by its guard — not as `NoMatch`
    /// (the row exists) and not as an internal error.
    #[tokio::test]
    async fn a_refusal_the_first_probe_missed_is_named_by_the_second() {
        let guards = [CapGuard::CountBelow {
            filters: Vec::new(),
            cap: 3,
        }];
        let inserted = DbExec::insert_guarded(&RacedGuardMock, "widgets", HashMap::new(), &guards)
            .await
            .expect("insert_guarded");
        assert!(
            matches!(inserted, GuardedInsert::Refused { guard: 0 }),
            "{inserted:?}"
        );
        let updated =
            DbExec::update_guarded(&RacedGuardMock, "widgets", &[], HashMap::new(), &guards)
                .await
                .expect("update_guarded");
        assert!(
            matches!(updated, GuardedUpdate::Refused { guard: 0 }),
            "{updated:?}"
        );
    }

    #[tokio::test]
    async fn batch_with_no_ops_runs_nothing() {
        let mock = BatchMock::new(0);
        assert!(DbExec::batch(&mock, Vec::new())
            .await
            .expect("empty batch")
            .is_empty());
        assert!(mock.tx_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_many_with_no_rows_issues_nothing() {
        let mock = BatchMock::new(0);
        assert_eq!(
            DbExec::create_many(&mock, "widgets", Vec::new())
                .await
                .expect("empty create_many"),
            0
        );
        assert!(mock.batch_calls.lock().unwrap().is_empty());
        assert!(mock.tx_calls.lock().unwrap().is_empty());
    }
}
