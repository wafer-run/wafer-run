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

use super::service::{
    AggregateSpec, DatabaseError, Record, RecordList, UpsertConflict, UpsertSpec,
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

/// Execution primitives + shared orchestration for SQL `DatabaseService` backends.
#[wafer_async_trait]
pub trait DbExec: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// SQL dialect this backend builds for (placeholder style, introspection).
    const BACKEND: Backend;

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

    // ---- Shared default methods (the dedup'd orchestration) ----
    // Named like the `DatabaseService` methods; the `DatabaseService` impl
    // forwards with explicit qualification (`DbExec::get(self, ...)`) to avoid
    // self-recursion.

    /// Column names (lowercased) of `table`; empty if the table is missing.
    ///
    /// Shared across backends via the parameter-bound
    /// [`introspect::build_list_columns`] builder, whose result shape (`name`
    /// per column) is identical in both dialects.
    async fn get_columns(&self, table: &str) -> Result<Vec<String>, DatabaseError> {
        let (sql, params) = introspect::build_list_columns(table, Self::BACKEND);
        let rows = self.run_fetch(&sql, &params).await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                r.data
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_lowercase)
            })
            .collect())
    }

    /// Add `column` to `table` via `stmt` unless a concurrent writer beat us
    /// to it.
    ///
    /// SQLite has no `ADD COLUMN IF NOT EXISTS`, so the callers' check-then-add
    /// has a race window between the existence check and the `ALTER`. On
    /// failure, re-check: a now-present column means another writer added it
    /// (benign); a still-missing column is a real DDL error and propagates.
    async fn add_column_checked(
        &self,
        table: &str,
        column: &str,
        stmt: &wafer_sql_utils::Statement,
    ) -> Result<(), DatabaseError> {
        if let Err(e) = self.run_execute(&stmt.sql, &[]).await {
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
        if !self.dbx_table_exists(&table).await? {
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

        let total_count: Option<i64> = match count_stmt {
            Some(stmt) => Some(
                self.run_scalar_i64(&stmt.sql, &sea_values_to_json(stmt.values))
                    .await?,
            ),
            None => None,
        };

        let records = self
            .run_fetch(&select_stmt.sql, &sea_values_to_json(select_stmt.values))
            .await?;

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
        if !self.dbx_table_exists(&table).await? {
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
        let stmt = wafer_sql_utils::query::build_update_by_id(&table, id, &pairs, Self::BACKEND);
        let affected = self
            .run_execute(&stmt.sql, &sea_values_to_json(stmt.values))
            .await?;
        if affected == 0 {
            return Err(DatabaseError::NotFound);
        }
        self.get(collection, id).await
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
        if !self.dbx_table_exists(&table).await? {
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
        if !self.dbx_table_exists(&table).await? {
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
        if !self.dbx_table_exists(&table).await? {
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
        if !self.dbx_table_exists(&table).await? {
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
    async fn exec_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        self.run_execute(query, args).await
    }

    /// Shared `schema_table_exists`: pass-through to `dbx_table_exists`
    /// (the primitive preserves each backend's error text).
    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        self.dbx_table_exists(name).await
    }
}
