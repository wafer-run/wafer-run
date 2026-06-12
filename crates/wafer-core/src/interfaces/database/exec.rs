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

use wafer_block::db::{Filter, ListOptions, SortField};
use wafer_block_macro::wafer_async_trait;
use wafer_sql_utils::{ddl, ident::sanitize_ident, introspect, value::sea_values_to_json, Backend};

use super::service::{DatabaseError, Record, RecordList};

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

    /// Lazily add TEXT columns for `filters`/`sort` fields missing from
    /// `table` (they default to NULL), so queries and filtered writes never
    /// fail with "no such column" for a field the schema simply hasn't seen
    /// yet.
    async fn ensure_query_columns(
        &self,
        table: &str,
        filters: &[Filter],
        sort: &[SortField],
    ) -> Result<(), DatabaseError> {
        let existing = self.get_columns(table).await?;
        let mut added: Vec<String> = Vec::new();
        let fields = filters
            .iter()
            .map(|f| f.field.as_str())
            .chain(sort.iter().map(|s| s.field.as_str()));
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

        self.ensure_query_columns(&table, &opts.filters, &opts.sort)
            .await?;

        let total_count: Option<i64> = if opts.skip_count {
            None
        } else {
            let count_stmt =
                wafer_sql_utils::aggregate::build_count(&table, &opts.filters, Self::BACKEND);
            Some(
                self.run_scalar_i64(&count_stmt.sql, &sea_values_to_json(count_stmt.values))
                    .await?,
            )
        };

        let stmt = wafer_sql_utils::query::build_select(&table, opts, Self::BACKEND);
        let records = self
            .run_fetch(&stmt.sql, &sea_values_to_json(stmt.values))
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
        self.ensure_query_columns(&table, filters, &[]).await?;
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
        self.ensure_query_columns(&table, filters, &[]).await?;
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
        self.ensure_query_columns(&table, filters, &[]).await?;
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
        self.ensure_query_columns(&table, filters, &[]).await?;
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
        self.ensure_query_columns(&table, filters, &[]).await?;
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
