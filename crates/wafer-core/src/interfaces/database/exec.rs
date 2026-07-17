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
use wafer_sql_utils::{ddl, ident::sanitize_ident, introspect, value::sea_values_to_json, Backend};

use super::{
    schema_cache::SchemaCache,
    service::{AggregateSpec, DatabaseError, Record, RecordList, UpsertConflict, UpsertSpec},
};

/// Sanitize keys and sort `data` into deterministic `(column, value)` pairs.
///
/// Sorted-key iteration keeps the generated INSERT/UPDATE shape stable across
/// process starts: `HashMap` order is randomized by `RandomState`, which would
/// otherwise produce N permutations of the same statement — each a distinct
/// cached prepared statement on the backend. Keys are ident-sanitized so the
/// statement references exactly the column names the lazy column-add step
/// creates.
fn sorted_pairs(data: &HashMap<String, serde_json::Value>) -> Vec<(String, serde_json::Value)> {
    let mut pairs: Vec<(String, serde_json::Value)> = data
        .iter()
        .map(|(k, v)| (sanitize_ident(k), v.clone()))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs
}

/// Recursively collect the `field` name of every [`FilterTree::Leaf`],
/// depth-first. Used by [`DbExec::ensure_query_columns`] so fields that only
/// appear inside a group (`All`/`Any`) — not the flat `opts.filters` list —
/// still get their lazy TEXT column added before the query runs.
fn tree_leaf_fields(nodes: &[FilterTree]) -> Vec<&str> {
    fn walk<'a>(node: &'a FilterTree, out: &mut Vec<&'a str>) {
        match node {
            FilterTree::Leaf(f) => out.push(f.field.as_str()),
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

/// Stamp `updated_at` (and on create, `created_at`) if the caller didn't.
fn stamp_timestamps(data: &mut HashMap<String, serde_json::Value>, include_created: bool) {
    let now = chrono::Utc::now().to_rfc3339();
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

/// Extract the `id` and `key` string values from an upsert `data` list for the
/// windowed-counter path.
///
/// The windowed-counter builder binds these positionally — a fresh per-call
/// row identifier and the conflict-target value — so both must be present and
/// string-typed. A missing or non-string entry is a caller error (surfaced as
/// [`DatabaseError`]), never a silent default.
fn extract_windowed_id_key(
    data: &[(String, serde_json::Value)],
) -> Result<(&str, &str), DatabaseError> {
    fn field<'a>(data: &'a [(String, serde_json::Value)], name: &str) -> Option<&'a str> {
        data.iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.as_str())
    }
    let id = field(data, "id").ok_or_else(|| {
        DatabaseError::Internal(
            "windowed-counter upsert requires a string `id` value in data".into(),
        )
    })?;
    let key = field(data, "key").ok_or_else(|| {
        DatabaseError::Internal(
            "windowed-counter upsert requires a string `key` value in data".into(),
        )
    })?;
    Ok((id, key))
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
    },
    /// Expected to return exactly one row; no rows → `NotFound` (like
    /// [`DbExec::run_fetch_one`]).
    FetchOne {
        /// Rendered SQL for this statement.
        sql: &'a str,
        /// Positional parameters, JSON-encoded as `sea_values_to_json` produces.
        params: &'a [serde_json::Value],
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
            BatchOp::Rows { sql, params }
            | BatchOp::FetchOne { sql, params }
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
    /// When `true`, the shared orchestration skips both the per-operation
    /// table-exists guard (migrations are authoritative — the table is assumed
    /// present) and the lazy column-add `ALTER TABLE` path (the migrated schema
    /// is trusted — no columns are synthesized), removing schema introspection
    /// from the hot path entirely. Default `false` preserves the self-healing
    /// lazy-schema behavior for development, tests, and other implementors.
    fn strict_schema(&self) -> bool {
        false
    }

    // ---- Primitives: the only backend-specific execution code ----
    // `params` is the JSON form produced by `sea_values_to_json(stmt.values)`;
    // each backend binds it natively. All callers pass single-statement SQL.

    /// Run a row-returning query and convert rows to `Record`s.
    async fn run_fetch(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError>;

    /// Run a query expected to return exactly one row; no rows → `NotFound`.
    async fn run_fetch_one(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Record, DatabaseError>;

    /// Run a non-row statement; returns the affected-row count.
    async fn run_execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError>;

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

    /// Run an INSERT, returning the backend-generated integer row id, if any.
    ///
    /// The default delegates to [`run_execute`](Self::run_execute) and reports
    /// no generated id — correct for backends where `create` synthesizes the
    /// id before inserting (Postgres). SQLite overrides this to hold its
    /// connection lock across `execute` + `last_insert_rowid()`, so the rowid
    /// returned for INTEGER-PRIMARY-KEY tables can't race a concurrent insert.
    async fn run_insert(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Option<i64>, DatabaseError> {
        self.run_execute(sql, params).await?;
        Ok(None)
    }

    /// Whether `table` generates its own primary key on insert, in which case
    /// `create` must not synthesize a UUID string id.
    ///
    /// Default `false` (Postgres: ids are always caller- or UUID-supplied).
    /// SQLite overrides this to detect `INTEGER PRIMARY KEY` autoincrement
    /// tables, whose ids come from [`run_insert`](Self::run_insert).
    async fn table_autogenerates_id(&self, _table: &str) -> bool {
        false
    }

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
                BatchOp::Rows { sql, params } => {
                    BatchResult::Rows(self.run_fetch(sql, params).await?)
                }
                BatchOp::FetchOne { sql, params } => {
                    BatchResult::FetchOne(self.run_fetch_one(sql, params).await?)
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
    /// the table is assumed present and no probe is issued. Otherwise returns
    /// the memoized [`schema_cache`](Self::schema_cache) fact on a hit, else
    /// probes once via [`dbx_table_exists`](Self::dbx_table_exists) and stores
    /// the result. The explicit [`schema_table_exists`](Self::schema_table_exists)
    /// API deliberately bypasses this and stays a live probe for callers that
    /// want ground truth.
    async fn table_present_for_op(&self, table: &str) -> Result<bool, DatabaseError> {
        if self.strict_schema() {
            return Ok(true);
        }
        let cache = self.schema_cache();
        if let Some(exists) = cache.and_then(|c| c.table_exists(table)) {
            return Ok(exists);
        }
        // Snapshot the generation *before* the probe yields; the gen-guarded
        // write-back below is dropped if a mutation raced the probe (see
        // `SchemaCache` docs). Holding `Option<&SchemaCache>` across the await
        // is fine — it is a plain reference, never a lock guard.
        let gen0 = cache.map(SchemaCache::generation);
        let exists = self.dbx_table_exists(table).await?;
        if let (Some(cache), Some(gen0)) = (cache, gen0) {
            cache.set_table_exists_if_gen(table, exists, gen0);
        }
        Ok(exists)
    }

    /// Column names (lowercased) of `table`; empty if the table is missing.
    ///
    /// Consults [`schema_cache`](Self::schema_cache) first and populates it on
    /// a miss, so a warm backend answers without a round-trip. Shared across
    /// backends via the parameter-bound [`introspect::build_list_columns`]
    /// builder, whose result shape (`name` per column) is identical in both
    /// dialects.
    async fn get_columns(&self, table: &str) -> Result<Vec<String>, DatabaseError> {
        let cache = self.schema_cache();
        if let Some(columns) = cache.and_then(|c| c.columns(table)) {
            return Ok(columns);
        }
        // Snapshot the generation before the probe yields; a mutation racing
        // the introspection drops the write-back rather than caching a stale
        // column set (see `SchemaCache` docs).
        let gen0 = cache.map(SchemaCache::generation);
        let (sql, params) = introspect::build_list_columns(table, Self::BACKEND);
        let rows = self.run_fetch(&sql, &params).await?;
        let columns: Vec<String> = rows
            .into_iter()
            .filter_map(|r| {
                r.data
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_lowercase)
            })
            .collect();
        if let (Some(cache), Some(gen0)) = (cache, gen0) {
            cache.set_columns_if_gen(table, columns.clone(), gen0);
        }
        Ok(columns)
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
                return Err(DatabaseError::Internal(format!("add column {column}: {e}")));
            }
        }
        Ok(())
    }

    /// Lazily add columns for every key in `data` missing from `table`.
    ///
    /// Column types are derived from the value being written
    /// ([`ddl::build_add_column_for_value`]): Postgres picks a native type
    /// (BOOLEAN/BIGINT/DOUBLE PRECISION/JSONB/TEXT), SQLite always TEXT. The
    /// table itself must already exist via the block's migration files — only
    /// columns are added on demand, per the documented lazy column-add design.
    async fn ensure_data_columns(
        &self,
        table: &str,
        data: &HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
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
            let safe_key = sanitize_ident(key);
            if existing.contains(&safe_key.to_lowercase()) {
                continue;
            }
            let stmt = ddl::build_add_column_for_value(table, &safe_key, &data[key], Self::BACKEND);
            self.add_column_checked(table, &safe_key, &stmt).await?;
        }
        Ok(())
    }

    /// Lazily add TEXT columns for `filters`/`sort`/`filter_tree` fields
    /// missing from `table` (they default to NULL), so queries and filtered
    /// writes never fail with "no such column" for a field the schema simply
    /// hasn't seen yet.
    ///
    /// `filter_tree` fields aren't included in `filters` — a group's leaves
    /// live only in the tree (see [`DbExec::list`]) — so `filter_tree` is
    /// walked separately via [`tree_leaf_fields`] to cover them too.
    async fn ensure_query_columns(
        &self,
        table: &str,
        filters: &[Filter],
        sort: &[SortField],
        filter_tree: Option<&[FilterTree]>,
    ) -> Result<(), DatabaseError> {
        // STRICT_SCHEMA trusts the migrated schema: skip the lazy TEXT-column
        // add (see [`ensure_data_columns`](Self::ensure_data_columns)).
        if self.strict_schema() {
            return Ok(());
        }
        let existing = self.get_columns(table).await?;
        let mut added: Vec<String> = Vec::new();
        let tree_fields = filter_tree.map(tree_leaf_fields).unwrap_or_default();
        let fields = filters
            .iter()
            .map(|f| f.field.as_str())
            .chain(sort.iter().map(|s| s.field.as_str()))
            .chain(tree_fields);
        for field in fields {
            let safe_field = sanitize_ident(field);
            let lower = safe_field.to_lowercase();
            if existing.contains(&lower) || added.contains(&lower) {
                continue;
            }
            let stmt = ddl::build_add_text_column(table, &safe_field, Self::BACKEND);
            self.add_column_checked(table, &safe_field, &stmt).await?;
            added.push(lower);
        }
        Ok(())
    }

    /// Shared `get`: select-by-id → single row.
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        let stmt = wafer_sql_utils::query::build_select_by_id(collection, id, Self::BACKEND);
        self.run_fetch_one(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Shared `list`: table-exists guard → ensure columns → optional count → select.
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
        let table = sanitize_ident(collection);
        if !self.table_present_for_op(&table).await? {
            return Ok(RecordList {
                records: Vec::new(),
                total_count: 0,
                page: 1,
                page_size: if opts.limit > 0 { opts.limit } else { 0 },
            });
        }

        self.ensure_query_columns(
            &table,
            &opts.filters,
            &opts.sort,
            opts.filter_tree.as_deref(),
        )
        .await?;

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
            let extra_cond = opts
                .filter_tree
                .as_deref()
                .and_then(wafer_sql_utils::query::build_condition_tree);

            let count_stmt = (!opts.skip_count).then(|| {
                wafer_sql_utils::aggregate::build_count_with_condition(
                    &table,
                    &opts.filters,
                    extra_cond.clone(),
                    Self::BACKEND,
                )
            });

            let select_stmt = match &opts.columns {
                Some(cols) => {
                    let refs: Vec<&str> = cols.iter().map(String::as_str).collect();
                    wafer_sql_utils::query::build_select_columns(
                        &table,
                        &refs,
                        opts,
                        extra_cond,
                        Self::BACKEND,
                    )
                }
                None => wafer_sql_utils::query::build_select_with_condition(
                    &table,
                    opts,
                    extra_cond,
                    Self::BACKEND,
                ),
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
                    .run_fetch(&select_stmt.sql, &sea_values_to_json(select_stmt.values))
                    .await?;
                (None, records)
            }
        };

        let page = if opts.limit > 0 {
            (opts.offset / opts.limit) + 1
        } else {
            1
        };
        let total_count = total_count.unwrap_or(records.len() as i64);
        Ok(RecordList {
            records,
            total_count,
            page,
            page_size: if opts.limit > 0 {
                opts.limit
            } else {
                total_count
            },
        })
    }

    /// Shared `count`: table-exists guard → ensure columns → COUNT(*).
    async fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError> {
        let table = sanitize_ident(collection);
        if !self.table_present_for_op(&table).await? {
            return Ok(0);
        }
        self.ensure_query_columns(&table, filters, &[], None)
            .await?;
        let stmt = wafer_sql_utils::aggregate::build_count(&table, filters, Self::BACKEND);
        self.run_scalar_i64(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Shared `sum`: SUM(field) with filters (no table-exists guard, no ensure).
    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        let table = sanitize_ident(collection);
        let stmt = wafer_sql_utils::aggregate::build_sum(&table, field, filters, Self::BACKEND);
        self.run_scalar_f64(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Shared `create`: id/timestamp defaulting → lazy column-add → INSERT.
    ///
    /// A missing `id` gets a synthesized UUID string unless the backend
    /// reports the table generates its own
    /// ([`table_autogenerates_id`](Self::table_autogenerates_id)), in which
    /// case the backend-generated id from [`run_insert`](Self::run_insert) is
    /// folded back into the returned record.
    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        let table = sanitize_ident(collection);
        let mut data = data;

        if !data.contains_key("id") && !self.table_autogenerates_id(&table).await {
            data.insert(
                "id".to_string(),
                serde_json::Value::String(uuid::Uuid::new_v4().to_string()),
            );
        }
        stamp_timestamps(&mut data, true);

        // Ensure any new columns exist. Table creation itself is the block
        // migration's job; a failure here is a real DDL error and propagates
        // rather than letting the INSERT fail with a confusing
        // "no such column".
        self.ensure_data_columns(&table, &data).await?;

        let pairs = sorted_pairs(&data);
        let stmt = wafer_sql_utils::query::build_insert(&table, &pairs, Self::BACKEND);
        let generated = self
            .run_insert(&stmt.sql, &sea_values_to_json(stmt.values))
            .await?;

        let id = match data.get("id") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => generated.map_or_else(String::new, |rowid| {
                // Autoincrement table: fold the generated id into the record.
                data.insert("id".to_string(), serde_json::json!(rowid));
                rowid.to_string()
            }),
        };
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
        let table = sanitize_ident(collection);
        let mut data = data;
        stamp_timestamps(&mut data, false);
        self.ensure_data_columns(&table, &data).await?;

        let pairs = sorted_pairs(&data);
        // Batch the UPDATE with the by-id re-fetch so a batching backend (D1)
        // collapses the two round-trips into one. The re-fetch mirrors
        // [`get`](Self::get) exactly — `build_select_by_id` on the raw
        // `collection`, decoded row-by-row. The `Execute` result stays the
        // authoritative existence check: 0 rows affected → `NotFound`, exactly
        // as the prior explicit affected-count guard, so the (discarded) select
        // rows are only read when the row actually existed. The sequential
        // default runs UPDATE then select one-by-one — observably identical to
        // the prior `run_execute` + `get` (empty select ⇒ `NotFound`, matching
        // `run_fetch_one`).
        let update_stmt =
            wafer_sql_utils::query::build_update_by_id(&table, id, &pairs, Self::BACKEND);
        let select_stmt = wafer_sql_utils::query::build_select_by_id(collection, id, Self::BACKEND);
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
        let stmt = wafer_sql_utils::query::build_delete_by_id(collection, id, Self::BACKEND);
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

    /// Shared `delete_where_count`: table-exists guard → lazy filter-column
    /// add → DELETE, returning the affected-row count (0 for a missing table).
    async fn delete_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        let table = sanitize_ident(collection);
        if !self.table_present_for_op(&table).await? {
            return Ok(0);
        }
        self.ensure_query_columns(&table, filters, &[], None)
            .await?;
        let stmt = wafer_sql_utils::query::build_delete_where(&table, filters, Self::BACKEND);
        self.run_execute(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Shared `take_where`: DELETE ... RETURNING the deleted rows; missing
    /// table → empty.
    async fn take_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError> {
        let table = sanitize_ident(collection);
        if !self.table_present_for_op(&table).await? {
            return Ok(Vec::new());
        }
        self.ensure_query_columns(&table, filters, &[], None)
            .await?;
        let stmt =
            wafer_sql_utils::query::build_delete_where_returning(&table, filters, Self::BACKEND);
        self.run_fetch(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Shared `update_where`: bulk UPDATE matching `filters`; missing table →
    /// `NotFound`. Lazily adds both the SET columns (typed from the data) and
    /// the filter columns.
    async fn update_where(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        let table = sanitize_ident(collection);
        if !self.table_present_for_op(&table).await? {
            return Err(DatabaseError::NotFound);
        }
        let mut data = data;
        stamp_timestamps(&mut data, false);
        self.ensure_data_columns(&table, &data).await?;
        self.ensure_query_columns(&table, filters, &[], None)
            .await?;
        let pairs = sorted_pairs(&data);
        let stmt =
            wafer_sql_utils::query::build_update_where(&table, &pairs, filters, Self::BACKEND);
        self.run_execute(&stmt.sql, &sea_values_to_json(stmt.values))
            .await?;
        Ok(())
    }

    /// Shared `update_where_count`: table-exists guard → lazy filter/data-column
    /// add → UPDATE, returning the affected-row count (0 for a missing table).
    async fn update_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<i64, DatabaseError> {
        let table = sanitize_ident(collection);
        if !self.table_present_for_op(&table).await? {
            return Ok(0);
        }
        let mut data = data;
        stamp_timestamps(&mut data, false);
        self.ensure_data_columns(&table, &data).await?;
        self.ensure_query_columns(&table, filters, &[], None)
            .await?;
        let pairs = sorted_pairs(&data);
        let stmt =
            wafer_sql_utils::query::build_update_where(&table, &pairs, filters, Self::BACKEND);
        self.run_execute(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Shared `increment_field_where`: single-statement atomic
    /// `SET col = col + delta` on matching rows, returning the affected-row
    /// count (0 for a missing table).
    async fn increment_field_where(
        &self,
        collection: &str,
        col: &str,
        delta: i64,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        let table = sanitize_ident(collection);
        if !self.table_present_for_op(&table).await? {
            return Ok(0);
        }
        self.ensure_query_columns(&table, filters, &[], None)
            .await?;
        let stmt = wafer_sql_utils::query::build_increment_field_where(
            &table,
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
    /// `DO NOTHING`). `WindowedCounter` reads the `id`/`key` insert values
    /// from `spec.data` (via [`extract_windowed_id_key`]) and renders the
    /// atomic windowed-counter statement, whose `created_fields` are stamped
    /// on INSERT only while `updated_fields` are re-stamped on conflict.
    ///
    /// Identifiers are validated at the trust boundary (the database handler's
    /// `to_upsert_spec`) before reaching here, and again inside
    /// `build_windowed_counter_upsert` — a fail-closed guard, since those
    /// column names are interpolated into `CASE`/`SET` expression text.
    ///
    /// For `WindowedCounter`, the handler's `to_upsert_spec` also already
    /// guarantees `spec.conflict_columns` is non-empty and that `spec.data`
    /// carries string `id`/`key` entries (both `InvalidArgument` at the
    /// handler boundary on a caller mistake); the `.first()`/
    /// `extract_windowed_id_key` handling below is a defensive fallback, not
    /// the primary validation.
    async fn upsert(&self, collection: &str, spec: UpsertSpec) -> Result<i64, DatabaseError> {
        let table = sanitize_ident(collection);
        let stmt = match spec.on_conflict {
            UpsertConflict::SetColumns(update_cols) => {
                let conflict: Vec<&str> =
                    spec.conflict_columns.iter().map(String::as_str).collect();
                let update: Vec<&str> = update_cols.iter().map(String::as_str).collect();
                wafer_sql_utils::upsert::build_upsert(
                    &table,
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
                let (id, key) = extract_windowed_id_key(&spec.data)?;
                let conflict_col = spec
                    .conflict_columns
                    .first()
                    .map(String::as_str)
                    .ok_or_else(|| {
                        DatabaseError::Internal(
                            "windowed-counter upsert requires a non-empty conflict_columns \
                         (should have been rejected as InvalidArgument at the handler \
                         boundary)"
                                .into(),
                        )
                    })?;
                let created: Vec<&str> = created_fields.iter().map(String::as_str).collect();
                let updated: Vec<&str> = updated_fields.iter().map(String::as_str).collect();
                wafer_sql_utils::upsert::build_windowed_counter_upsert(
                    &table,
                    conflict_col,
                    id,
                    key,
                    &count_field,
                    &window_field,
                    &created,
                    &updated,
                    now,
                    window_cutoff,
                    Self::BACKEND,
                )
                .map_err(|e| DatabaseError::Internal(e.to_string()))?
            }
        };
        self.run_execute(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
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
    /// Identifiers are validated at the trust boundary (the handler's
    /// `to_aggregate_spec`) before reaching here.
    async fn aggregate(
        &self,
        collection: &str,
        spec: AggregateSpec,
    ) -> Result<Vec<Record>, DatabaseError> {
        let table = sanitize_ident(collection);
        let stmt = {
            let cfg = spec.into_grouped_config(table);
            wafer_sql_utils::aggregate::build_grouped_query(cfg, Self::BACKEND)
        };
        self.run_fetch(&stmt.sql, &sea_values_to_json(stmt.values))
            .await
    }

    /// Shared `query_raw`: pass-through to `run_fetch`.
    async fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        self.run_fetch(query, args).await
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

    /// Shared `schema_table_exists`: pass-through to `dbx_table_exists`
    /// (the primitive preserves each backend's error text).
    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        self.dbx_table_exists(name).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Notify;

    use super::*;

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

        fn schema_cache(&self) -> Option<&SchemaCache> {
            Some(&self.cache)
        }

        async fn run_fetch(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<Vec<Record>, DatabaseError> {
            Ok(Vec::new())
        }

        async fn run_fetch_one(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
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
            // Announce that we've captured gen0 and are parked in the probe,
            // then wait to be released — the window an invalidation must win.
            self.entered_probe.notify_one();
            self.release_probe.notified().await;
            Ok(self.exists)
        }
    }

    /// Exec-level proof of the linearizability fix: when an invalidation lands
    /// while `table_present_for_op` is parked in its probe, the stale
    /// `exists=false` read (taken just before a concurrent CREATE) is NOT
    /// written back — the next op re-probes instead of trusting a resurrected
    /// negative cache (Repro A).
    #[tokio::test]
    async fn probe_write_back_dropped_when_invalidated_mid_flight() {
        let backend = BarrierExec {
            cache: SchemaCache::new(),
            entered_probe: Arc::new(Notify::new()),
            release_probe: Arc::new(Notify::new()),
            exists: false,
        };
        let entered = backend.entered_probe.clone();
        let release = backend.release_probe.clone();

        let probe = backend.table_present_for_op("orders");
        let racer = async {
            // Wait until the probe has snapshotted gen0 and parked in the DB call.
            entered.notified().await;
            // A concurrent migration CREATEs the table and invalidates the cache
            // (bumping the generation past the probe's snapshot).
            backend.cache.invalidate("orders");
            // Release the probe to attempt its now-stale write-back.
            release.notify_one();
        };

        let (present, ()) = tokio::join!(probe, racer);
        // The probe still returns what the DB told it at read time...
        assert!(
            !present.expect("probe succeeds"),
            "probe returns its read-time value"
        );
        // ...but that stale value must NOT have been cached.
        assert_eq!(
            backend.cache.table_exists("orders"),
            None,
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

        async fn run_fetch(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<Vec<Record>, DatabaseError> {
            self.record(format!("fetch:{sql}"));
            Ok(vec![Self::record_row(sql)])
        }

        async fn run_fetch_one(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
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
                },
                BatchOp::Execute {
                    sql: "FAIL",
                    params: &[],
                },
                BatchOp::Rows {
                    sql: "C",
                    params: &[],
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

    #[test]
    fn batch_op_sql_params_returns_the_pair_for_every_variant() {
        let params = vec![serde_json::json!("x")];
        for op in [
            BatchOp::Rows {
                sql: "s",
                params: &params,
            },
            BatchOp::FetchOne {
                sql: "s",
                params: &params,
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
        fetch_calls: Mutex<Vec<String>>,
    }

    impl BatchMock {
        fn new(canned_count: i64) -> Self {
            Self {
                canned_count,
                batch_calls: Mutex::new(Vec::new()),
                fetch_calls: Mutex::new(Vec::new()),
            }
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

        async fn run_fetch(
            &self,
            sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<Vec<Record>, DatabaseError> {
            self.fetch_calls.lock().unwrap().push(sql.to_string());
            Ok(Self::canned_rows())
        }

        async fn run_fetch_one(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
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

        async fn run_batch(&self, ops: &[BatchOp<'_>]) -> Result<Vec<BatchResult>, DatabaseError> {
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

        // total_count falls back to records.len() when the count is skipped.
        assert_eq!(list.records.len(), 2);
        assert_eq!(list.total_count, 2);
    }
}
