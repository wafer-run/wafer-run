use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
// Import query types from wafer-block for use in trait method signatures.
use wafer_block::db::{Filter, FilterTree, ListOptions, SortField};
use wafer_block_macro::wafer_async_trait;
// Re-export schema types so consumers access them through the database module.
pub use wafer_schema::{
    col_blob, col_bool, col_datetime, col_float, col_int, col_int64, col_json, col_string,
    col_text, default_empty, default_false, default_int, default_now, default_null, default_string,
    default_true, default_zero, pk, pk_int, soft_delete as schema_soft_delete, timestamps, Column,
    DataType, DefaultVal, DefaultValue, Index, Reference, Table,
};

/// Errors returned by [`DatabaseService`] operations.
#[derive(Error, Debug)]
pub enum DatabaseError {
    /// No record with the requested id exists.
    #[error("record not found")]
    NotFound,
    /// Backend-internal failure.
    #[error("database error: {0}")]
    Internal(String),
    /// Wrapped foreign error from a backend driver.
    #[error("{0}")]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

/// Plain-data upsert specification handed to [`DatabaseService::upsert`].
///
/// The database handler converts the wire
/// [`UpsertRequest`](wafer_block::wire::database::UpsertRequest) into this,
/// validating every identifier that reaches raw SQL text — so the service
/// never sees an untrusted column name.
#[derive(Debug, Clone)]
pub struct UpsertSpec {
    /// Insert column → value pairs (order preserved for deterministic SQL).
    pub data: Vec<(String, serde_json::Value)>,
    /// Conflict-target columns (must carry a `UNIQUE`/`PRIMARY KEY` constraint).
    pub conflict_columns: Vec<String>,
    /// Conflict-resolution strategy.
    pub on_conflict: UpsertConflict,
}

/// Builder-input twin of the wire
/// [`OnConflict`](wafer_block::wire::database::OnConflict) — the plain-data
/// form consumed by [`UpsertSpec`]. See the wire type for full semantics.
#[derive(Debug, Clone)]
pub enum UpsertConflict {
    /// `DO UPDATE SET <cols> = excluded.<cols>` (empty ⇒ `DO NOTHING`).
    SetColumns(Vec<String>),
    /// Atomic sliding-window counter.
    WindowedCounter {
        /// Counter column.
        count_field: String,
        /// Window-start column.
        window_field: String,
        /// Current epoch-seconds.
        now: i64,
        /// Expiry cutoff (`now - window_secs`).
        window_cutoff: i64,
        /// Creation-timestamp columns (stamped on INSERT only).
        created_fields: Vec<String>,
        /// Modification-timestamp columns (stamped on INSERT and on conflict).
        updated_fields: Vec<String>,
    },
}

/// Plain-data grouped-aggregate specification handed to
/// [`DatabaseService::aggregate`].
///
/// The database handler converts the wire
/// [`AggregateRequest`](wafer_block::wire::database::AggregateRequest) into
/// this, validating **every** identifier that reaches raw SQL text (aliases,
/// aggregated `field`s, date-bucket fields, plain group-by columns, and
/// `select_columns`) and bounding every `CaseWhenSum` predicate tree — so the
/// service never sees an untrusted column name. Rendering into the `!Send`
/// [`GroupedQueryConfig`](wafer_sql_utils::aggregate::GroupedQueryConfig)
/// happens server-side in [`DbExec::aggregate`](super::exec::DbExec::aggregate)
/// via [`AggregateSpec::into_grouped_config`].
#[derive(Debug, Clone)]
pub struct AggregateSpec {
    /// Plain (non-aggregated) columns to also select.
    pub select_columns: Vec<String>,
    /// Aggregate output columns (order preserved).
    pub aggregates: Vec<AggregateColumnSpec>,
    /// `WHERE` predicates, AND-combined leaves (groups rejected upstream).
    pub filters: Vec<Filter>,
    /// `GROUP BY` terms — plain columns and/or date buckets.
    pub group_by: Vec<GroupBySpec>,
    /// `ORDER BY` clause (aggregate aliases are valid sort keys).
    pub sort: Vec<SortField>,
    /// Optional `LIMIT N`; a value `<= 0` means no limit.
    pub limit: i64,
}

/// One aggregate output column in an [`AggregateSpec`] — the validated,
/// plain-data twin of the wire
/// [`AggregateColumnDef`](wafer_block::wire::database::AggregateColumnDef).
///
/// The `CaseWhenSum` predicate is carried as an already-bounds-checked
/// [`FilterTree`] forest (not a sea-query `SimpleExpr`) so the `!Send` `CASE`
/// expression can be built server-side in
/// [`AggregateSpec::into_grouped_config`].
#[derive(Debug, Clone)]
pub enum AggregateColumnSpec {
    /// `COUNT(*) AS alias`.
    Count {
        /// Output alias.
        alias: String,
    },
    /// `SUM(field) AS alias`.
    Sum {
        /// Numeric column to sum.
        field: String,
        /// Output alias.
        alias: String,
    },
    /// `AVG(field) AS alias`.
    Avg {
        /// Numeric column to average.
        field: String,
        /// Output alias.
        alias: String,
    },
    /// `MAX(field) AS alias`.
    Max {
        /// Column to take the maximum of.
        field: String,
        /// Output alias.
        alias: String,
    },
    /// `SUM(CASE WHEN <when> THEN 1 ELSE 0 END) AS alias` — a portable
    /// conditional count. `when` is the validated predicate forest,
    /// AND-combined at the top level.
    CaseWhenSum {
        /// Predicate whose matching rows are counted.
        when: Vec<FilterTree>,
        /// Output alias.
        alias: String,
    },
}

/// One `GROUP BY` term in an [`AggregateSpec`]: a plain column or a date
/// bucket. Plain-data twin of the wire
/// [`GroupByDef`](wafer_block::wire::database::GroupByDef).
#[derive(Debug, Clone)]
pub enum GroupBySpec {
    /// Group by a plain column.
    Column(String),
    /// Group by the date bucket `date(field)`; the bucketed value is emitted
    /// in each result row under the `field` name.
    DateBucket {
        /// Timestamp column to bucket by day.
        field: String,
    },
}

impl AggregateSpec {
    /// Render this validated spec into a
    /// [`GroupedQueryConfig`](wafer_sql_utils::aggregate::GroupedQueryConfig)
    /// for `table`.
    ///
    /// Builds the `!Send` sea-query expressions (the `CaseWhenSum` `CASE`
    /// predicate via [`wafer_sql_utils::query::tree_to_simple_expr`]); the
    /// returned config holds `Rc<dyn Iden>` and is therefore also `!Send`, so
    /// call this server-side inside
    /// [`DbExec::aggregate`](super::exec::DbExec::aggregate) and drop the
    /// result before the next `.await`.
    #[must_use]
    pub fn into_grouped_config(
        self,
        table: String,
    ) -> wafer_sql_utils::aggregate::GroupedQueryConfig {
        use wafer_sql_utils::aggregate::{
            AggFunc, AggregateColumn, DateBucketGroup, GroupedQueryConfig,
        };

        let aggregates = self
            .aggregates
            .into_iter()
            .map(|a| match a {
                AggregateColumnSpec::Count { alias } => AggregateColumn {
                    func: AggFunc::Count,
                    field: None,
                    alias,
                    cast_as: None,
                    inner_expr: None,
                },
                AggregateColumnSpec::Sum { field, alias } => AggregateColumn {
                    func: AggFunc::Sum,
                    field: Some(field),
                    alias,
                    cast_as: None,
                    inner_expr: None,
                },
                AggregateColumnSpec::Avg { field, alias } => AggregateColumn {
                    func: AggFunc::Avg,
                    field: Some(field),
                    alias,
                    cast_as: None,
                    inner_expr: None,
                },
                AggregateColumnSpec::Max { field, alias } => AggregateColumn {
                    func: AggFunc::Max,
                    field: Some(field),
                    alias,
                    cast_as: None,
                    inner_expr: None,
                },
                // The `when` predicate is `!Send` once turned into a
                // `SimpleExpr`, so it is built here (server-side), never in the
                // handler.
                AggregateColumnSpec::CaseWhenSum { when, alias } => AggregateColumn::case_when_sum(
                    alias,
                    wafer_sql_utils::query::tree_to_simple_expr(&when),
                ),
            })
            .collect();

        // Plain group-by columns render as quoted identifiers; date buckets
        // render `date(field)` (and also select the bucketed value) via the
        // builder's shared per-dialect date expression.
        let mut group_by = Vec::new();
        let mut date_buckets = Vec::new();
        for g in self.group_by {
            match g {
                GroupBySpec::Column(c) => group_by.push(c),
                GroupBySpec::DateBucket { field } => date_buckets.push(DateBucketGroup {
                    alias: field.clone(),
                    field,
                }),
            }
        }

        GroupedQueryConfig {
            table,
            select_columns: self.select_columns,
            aggregates,
            filters: self.filters,
            group_by,
            date_buckets,
            order_by: self.sort,
            limit: if self.limit > 0 {
                Some(self.limit)
            } else {
                None
            },
        }
    }
}

/// Service provides generic CRUD operations on collections.
#[wafer_async_trait]
pub trait DatabaseService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// Get retrieves a single record by ID from a collection.
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError>;

    /// List retrieves records with optional filtering, sorting, and pagination.
    async fn list(&self, collection: &str, opts: &ListOptions)
        -> Result<RecordList, DatabaseError>;

    /// Create inserts a new record into a collection.
    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError>;

    /// Update modifies an existing record by ID.
    async fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError>;

    /// Delete removes a record by ID.
    async fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError>;

    /// Count returns the number of records matching the filters.
    async fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError>;

    /// Sum returns the sum of a numeric field for matching records.
    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError>;

    /// QueryRaw executes a raw SELECT query.
    async fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError>;

    /// ExecRaw executes a raw non-SELECT statement.
    async fn exec_raw(&self, query: &str, args: &[serde_json::Value])
        -> Result<i64, DatabaseError>;

    /// Bulk-delete all records matching filters in a single query.
    async fn delete_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<(), DatabaseError> {
        // Default implementation falls back to record-by-record deletion.
        // Loops until all matching records are deleted.
        loop {
            let records = self
                .list(
                    collection,
                    &ListOptions {
                        filters: filters.to_vec(),
                        limit: 10000,
                        ..Default::default()
                    },
                )
                .await?;
            if records.records.is_empty() {
                break;
            }
            for r in records.records {
                self.delete(collection, &r.id).await?;
            }
        }
        Ok(())
    }

    /// Bulk-delete all records matching filters and return the number of deleted rows.
    ///
    /// Default impl: count then delete. Small TOCTOU window — concurrent inserts
    /// matching the filters may be deleted without being counted, or vice versa.
    /// Native sqlite/postgres impls override with a single DELETE statement that
    /// returns the affected-row count atomically.
    async fn delete_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        let n = self.count(collection, filters).await?;
        self.delete_where(collection, filters).await?;
        Ok(n)
    }

    /// Atomically select and delete all records matching filters, returning the
    /// deleted rows.
    ///
    /// Default impl: list then delete-by-id. Not atomic — concurrent writes to
    /// matching rows may race between the list and the deletes. Native
    /// sqlite/postgres impls override with `DELETE … WHERE … RETURNING *` (one
    /// statement, atomic).
    async fn take_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError> {
        let listed = self
            .list(
                collection,
                &ListOptions {
                    filters: filters.to_vec(),
                    limit: 10_000,
                    ..Default::default()
                },
            )
            .await?;
        let ids: Vec<_> = listed.records.iter().map(|r| r.id.clone()).collect();
        for id in &ids {
            self.delete(collection, id).await?;
        }
        Ok(listed.records)
    }

    /// Bulk-update all records matching filters in a single query.
    async fn update_where(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        // Default implementation falls back to record-by-record updates.
        let records = self
            .list(
                collection,
                &ListOptions {
                    filters: filters.to_vec(),
                    limit: 10000,
                    ..Default::default()
                },
            )
            .await?;

        let mut ids: Vec<String> = records.records.into_iter().map(|r| r.id).collect();
        if let Some(last_id) = ids.pop() {
            // Clone data for all but the last record.
            for id in &ids {
                self.update(collection, id, data.clone()).await?;
            }
            // Move data into the final update to avoid an extra clone.
            self.update(collection, &last_id, data).await?;
        }
        Ok(())
    }

    /// Bulk-update all records matching filters and return the number of updated rows.
    ///
    /// Default impl: count then update. Small TOCTOU window — concurrent writes
    /// to matching rows may be updated without being counted, or vice versa.
    /// Native sqlite/postgres impls override with a single UPDATE statement that
    /// returns the affected-row count atomically.
    async fn update_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<i64, DatabaseError> {
        let n = self.count(collection, filters).await?;
        self.update_where(collection, filters, data).await?;
        Ok(n)
    }

    /// Atomically increment `col` by `delta` on every row in `collection`
    /// matching `filters`. Returns the number of rows modified. Use a negative
    /// `delta` to decrement.
    ///
    /// Implementations must perform this as a single
    /// `UPDATE … SET col = col + delta WHERE …` round-trip — the whole point
    /// of this op is the absence of a read-modify-write race. The default
    /// here returns an `Internal` error so backends are forced to override.
    async fn increment_field_where(
        &self,
        _collection: &str,
        _col: &str,
        _delta: i64,
        _filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        Err(DatabaseError::Internal(
            "increment_field_where is not implemented by this database backend".into(),
        ))
    }

    /// Upsert `spec` into `collection`. SQL backends implement this via
    /// `DbExec::upsert`; a backend that cannot express `ON CONFLICT` must
    /// return an explicit error. No default — every backend states its choice.
    async fn upsert(&self, collection: &str, spec: UpsertSpec) -> Result<i64, DatabaseError>;

    /// Grouped aggregate query. SQL backends implement this via
    /// `DbExec::aggregate`. No default — every backend states its choice.
    async fn aggregate(
        &self,
        collection: &str,
        spec: AggregateSpec,
    ) -> Result<Vec<Record>, DatabaseError>;

    // --- Schema management methods ---

    /// Ensure a table exists matching the given schema definition.
    /// Creates the table if it doesn't exist and adds any missing columns.
    async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError>;

    /// Ensure multiple tables exist matching the given schema definitions.
    async fn ensure_schema_tables(&self, tables: &[Table]) -> Result<(), DatabaseError> {
        for t in tables {
            self.ensure_schema_table(t).await?;
        }
        Ok(())
    }

    /// Check whether a table exists in the database.
    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError>;

    /// Drop a table if it exists.
    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError>;

    /// Add a column to an existing table.
    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError>;
}

/// Record represents a single database record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    /// Primary-key identifier as text.
    pub id: String,
    /// Remaining columns rendered as a JSON-valued map.
    pub data: HashMap<String, serde_json::Value>,
}

/// RecordList represents a paginated list of records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordList {
    /// Records on the current page.
    pub records: Vec<Record>,
    /// Total matching rows across all pages (may be `records.len()` when count is skipped).
    pub total_count: i64,
    /// 1-based page index of this result set.
    pub page: i64,
    /// Maximum rows per page used to compute `page`.
    pub page_size: i64,
}
