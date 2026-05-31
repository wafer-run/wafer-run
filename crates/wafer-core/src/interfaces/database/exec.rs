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

use wafer_block::db::{Filter, ListOptions, SortField};
use wafer_block_macro::wafer_async_trait;
use wafer_sql_utils::{ident::sanitize_ident, value::sea_values_to_json, Backend};

use super::service::{DatabaseError, Record, RecordList};

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

    /// Ensure columns referenced by `filters`/`sort` exist on `table`.
    ///
    /// Backend hook so `list`/`count` share orchestration while each backend
    /// keeps its own error policy (SQLite swallows `ALTER` errors; Postgres
    /// propagates them).
    async fn ensure_query_columns(
        &self,
        table: &str,
        filters: &[Filter],
        sort: &[SortField],
    ) -> Result<(), DatabaseError>;

    // ---- Shared default methods (the dedup'd orchestration) ----
    // Named like the `DatabaseService` methods; the `DatabaseService` impl
    // forwards with explicit qualification (`DbExec::get(self, ...)`) to avoid
    // self-recursion.

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
