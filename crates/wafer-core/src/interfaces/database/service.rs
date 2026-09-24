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
use wafer_sql_utils::aggregate::CastType;
/// A count or sum cap a guarded write must stay within — see
/// [`DatabaseService::insert_guarded`].
pub use wafer_sql_utils::guard::CapGuard;

/// Errors returned by [`DatabaseService`] operations.
#[derive(Error, Debug)]
pub enum DatabaseError {
    /// No record with the requested id exists.
    #[error("record not found")]
    NotFound,
    /// A write would duplicate a primary or unique key. Every backend maps
    /// its driver's unique-violation to this — SQLite's
    /// `SQLITE_CONSTRAINT_UNIQUE`/`_PRIMARYKEY`, PostgreSQL's SQLSTATE
    /// `23505`, and an adapter whose driver reports it only as text (D1:
    /// `UNIQUE constraint failed`) by matching that text — so callers can
    /// tell "taken" from a fault. Other constraint violations stay `Internal`.
    #[error("unique constraint violated: {0}")]
    AlreadyExists(String),
    /// The request names a table or column the executor refuses (one that is
    /// not a plain identifier, or a column the table does not have), or asks
    /// for a page it cannot render (a zero limit, an offset with no limit).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// A fault in reaching or using the database that says nothing about the
    /// request and may clear on its own — a busy or locked SQLite file, a
    /// refused or broken connection, a pool with no connection free, a
    /// transaction the server rolled back to break a deadlock. Retrying the
    /// same request later may succeed. Each backend classifies its driver's
    /// errors into this variant; see its error mapping for the exact set.
    #[error("database unavailable: {0}")]
    Unavailable(String),
    /// Backend-internal failure.
    #[error("database error: {0}")]
    Internal(String),
    /// Wrapped foreign error from a backend driver.
    #[error("{0}")]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

impl DatabaseError {
    /// The wire [`ErrorCode`](wafer_block::ErrorCode) this error answers with:
    /// `NotFound`, `AlreadyExists` and `InvalidArgument` for their variants,
    /// `Unavailable` for a transient fault (so a caller, and the runtime for
    /// a failed block Init, may retry), `Internal` otherwise.
    #[must_use]
    pub const fn code(&self) -> wafer_block::ErrorCode {
        use wafer_block::ErrorCode;
        match self {
            Self::NotFound => ErrorCode::NotFound,
            Self::AlreadyExists(_) => ErrorCode::AlreadyExists,
            Self::InvalidArgument(_) => ErrorCode::InvalidArgument,
            Self::Unavailable(_) => ErrorCode::Unavailable,
            Self::Internal(_) | Self::Other(_) => ErrorCode::Internal,
        }
    }
}

/// A statement a builder refused to render is the caller's mistake: a name
/// that is not a plain identifier, a foreign-key action off the allowlist, a
/// limit/offset pair no backend can render, or one column named for two roles.
impl From<wafer_sql_utils::SqlBuildError> for DatabaseError {
    fn from(e: wafer_sql_utils::SqlBuildError) -> Self {
        Self::InvalidArgument(e.to_string())
    }
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

/// One write in a [`DatabaseService::batch`] — the plain-data twin of the wire
/// [`BatchWrite`](wafer_block::wire::database::BatchWrite).
///
/// Each variant has the semantics of the single-op method it is named after,
/// with one difference: an `Update` or `Delete` whose `id` matches no row is
/// not an error inside a batch, it is reported in that op's [`WriteOutcome`].
/// Only a failing statement aborts a batch.
#[derive(Debug, Clone)]
pub enum WriteOp {
    /// Insert a row, as [`DatabaseService::create`].
    Create {
        /// Target collection.
        collection: String,
        /// Column → value map; `id` and timestamps are stamped when absent.
        data: HashMap<String, serde_json::Value>,
    },
    /// Update one row by id, as [`DatabaseService::update`].
    Update {
        /// Target collection.
        collection: String,
        /// Primary-key id of the row.
        id: String,
        /// Column → value map to set.
        data: HashMap<String, serde_json::Value>,
    },
    /// Delete one row by id, as [`DatabaseService::delete`].
    Delete {
        /// Target collection.
        collection: String,
        /// Primary-key id of the row.
        id: String,
    },
    /// Update every row matching `filters`, as
    /// [`DatabaseService::update_where_count`].
    UpdateWhere {
        /// Target collection.
        collection: String,
        /// AND-combined predicates.
        filters: Vec<Filter>,
        /// Column → value map to set.
        data: HashMap<String, serde_json::Value>,
    },
    /// Insert-or-resolve one row, as [`DatabaseService::upsert`].
    Upsert {
        /// Target collection.
        collection: String,
        /// Validated upsert specification.
        spec: UpsertSpec,
    },
}

impl WriteOp {
    /// The collection this write targets — what the handler authorizes.
    #[must_use]
    pub fn collection(&self) -> &str {
        match self {
            Self::Create { collection, .. }
            | Self::Update { collection, .. }
            | Self::Delete { collection, .. }
            | Self::UpdateWhere { collection, .. }
            | Self::Upsert { collection, .. } => collection,
        }
    }
}

/// The result of one [`WriteOp`], in the same position as the op.
#[derive(Debug, Clone)]
pub enum WriteOutcome {
    /// The inserted row as stored, including its id.
    Created(Record),
    /// The updated row, or `None` when the id matched no row.
    Updated(Option<Record>),
    /// Rows deleted: `1`, or `0` when the id matched no row.
    Deleted {
        /// Number of rows deleted.
        rows_affected: i64,
    },
    /// Rows the filtered update changed.
    UpdatedWhere {
        /// Number of rows updated.
        rows_affected: i64,
    },
    /// Rows the upsert inserted or updated.
    Upserted {
        /// Rows affected by the insert/update.
        rows_affected: i64,
    },
}

/// What [`DatabaseService::insert_guarded`] did.
#[derive(Debug, Clone)]
pub enum GuardedInsert {
    /// Every guard held; the row as stored.
    Inserted(Record),
    /// The guard at this index of the call's `guards` refused the insert (the
    /// first that did, when several would); nothing was written.
    Refused {
        /// Index into the call's `guards`.
        guard: usize,
    },
}

/// What [`DatabaseService::update_guarded`] did.
#[derive(Debug, Clone)]
pub enum GuardedUpdate {
    /// Every guard held and this many rows matched and were updated (at
    /// least one).
    Updated {
        /// Rows updated.
        rows_affected: i64,
    },
    /// The guard at this index of the call's `guards` refused the update (the
    /// first that did); nothing was written. Guards are checked before the
    /// filters, so a refused update may also have matched no row.
    Refused {
        /// Index into the call's `guards`.
        guard: usize,
    },
    /// Every guard held but no row matched the filters (or the table does not
    /// exist); nothing was written.
    NoMatch,
}

/// Plain-data grouped-aggregate specification handed to
/// [`DatabaseService::aggregate`].
///
/// The database handler converts the wire
/// [`AggregateRequest`](wafer_block::wire::database::AggregateRequest) into
/// this, validating **every** identifier that reaches raw SQL text (aliases,
/// aggregated `field`s, date-bucket fields, plain group-by columns, and
/// `select_columns`), bounding every `CaseWhenSum` / `SumWhere` predicate tree,
/// and parsing every output cast against the
/// [`CastType`](wafer_sql_utils::aggregate::CastType) allowlist — so the
/// service never sees an untrusted column name or type name. Rendering into the `!Send`
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
/// The `CaseWhenSum` / `SumWhere` predicates are carried as already-bounds-checked
/// [`FilterTree`] forests (not sea-query `SimpleExpr`s) so the `!Send` `CASE`
/// expressions can be built server-side in
/// [`AggregateSpec::into_grouped_config`].
#[derive(Debug, Clone)]
pub enum AggregateColumnSpec {
    /// `COUNT(*) AS alias`.
    Count {
        /// Output alias.
        alias: String,
    },
    /// `SUM(field) AS alias`, cast when `cast_as` is set.
    Sum {
        /// Numeric column to sum.
        field: String,
        /// Output alias.
        alias: String,
        /// Optional output cast.
        cast_as: Option<CastType>,
    },
    /// `AVG(field) AS alias`, cast when `cast_as` is set. The database
    /// handler admits only [`CastType::Double`] here: `BigInt` rounds an
    /// average on Postgres and truncates it on SQLite.
    Avg {
        /// Numeric column to average.
        field: String,
        /// Output alias.
        alias: String,
        /// Optional output cast.
        cast_as: Option<CastType>,
    },
    /// `MAX(field) AS alias`.
    Max {
        /// Column to take the maximum of.
        field: String,
        /// Output alias.
        alias: String,
    },
    /// `COALESCE(SUM(CASE WHEN <when> THEN 1 ELSE 0 END), 0) AS alias` — a
    /// portable conditional count, `0` when no row matches. `when` is the
    /// validated predicate forest, AND-combined at the top level.
    CaseWhenSum {
        /// Predicate whose matching rows are counted.
        when: Vec<FilterTree>,
        /// Output alias.
        alias: String,
    },
    /// `COALESCE(SUM(CASE WHEN <when> THEN field ELSE 0 END), 0) AS alias` —
    /// the sum of `field` over the rows matching the validated predicate
    /// forest `when` (AND-combined at the top level), `0` when nothing
    /// non-null is summed, cast when `cast_as` is set.
    SumWhere {
        /// Numeric column to sum over the matching rows.
        field: String,
        /// Predicate selecting the rows whose `field` is summed.
        when: Vec<FilterTree>,
        /// Output alias.
        alias: String,
        /// Optional output cast.
        cast_as: Option<CastType>,
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
    /// The output aliases of [`aggregates`](Self::aggregates).
    #[must_use]
    pub fn aliases(&self) -> Vec<&str> {
        self.aggregates
            .iter()
            .map(|a| match a {
                AggregateColumnSpec::Count { alias }
                | AggregateColumnSpec::Sum { alias, .. }
                | AggregateColumnSpec::Avg { alias, .. }
                | AggregateColumnSpec::Max { alias, .. }
                | AggregateColumnSpec::CaseWhenSum { alias, .. }
                | AggregateColumnSpec::SumWhere { alias, .. } => alias.as_str(),
            })
            .collect()
    }

    /// Every table column the query reads: the selected and grouped columns,
    /// each aggregated `field`, the columns of every `when` predicate and of
    /// the filters. [`sort`](Self::sort) keys are not included — one may name
    /// an alias instead of a column.
    #[must_use]
    pub fn read_columns(&self) -> Vec<&str> {
        fn tree_columns<'a>(nodes: &'a [FilterTree], out: &mut Vec<&'a str>) {
            for node in nodes {
                match node {
                    FilterTree::Leaf(f) => out.push(f.field.as_str()),
                    FilterTree::ColumnCompare(f) => {
                        out.push(f.field.as_str());
                        out.push(f.column.as_str());
                    }
                    FilterTree::All(children) | FilterTree::Any(children) => {
                        tree_columns(children, out);
                    }
                }
            }
        }
        let mut out: Vec<&str> = self.select_columns.iter().map(String::as_str).collect();
        for aggregate in &self.aggregates {
            match aggregate {
                AggregateColumnSpec::Count { .. } => {}
                AggregateColumnSpec::Sum { field, .. }
                | AggregateColumnSpec::Avg { field, .. }
                | AggregateColumnSpec::Max { field, .. } => out.push(field),
                AggregateColumnSpec::CaseWhenSum { when, .. } => tree_columns(when, &mut out),
                AggregateColumnSpec::SumWhere { field, when, .. } => {
                    out.push(field);
                    tree_columns(when, &mut out);
                }
            }
        }
        for group in &self.group_by {
            match group {
                GroupBySpec::Column(column) => out.push(column),
                GroupBySpec::DateBucket { field } => out.push(field),
            }
        }
        out.extend(self.filters.iter().map(|f| f.field.as_str()));
        out
    }

    /// Render this validated spec into a
    /// [`GroupedQueryConfig`](wafer_sql_utils::aggregate::GroupedQueryConfig)
    /// for `table`.
    ///
    /// Builds the `!Send` sea-query expressions (the `CaseWhenSum` and
    /// `SumWhere` `CASE` predicates via [`wafer_sql_utils::query::tree_to_simple_expr`]); the
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
                AggregateColumnSpec::Sum {
                    field,
                    alias,
                    cast_as,
                } => AggregateColumn {
                    func: AggFunc::Sum,
                    field: Some(field),
                    alias,
                    cast_as,
                    inner_expr: None,
                },
                AggregateColumnSpec::Avg {
                    field,
                    alias,
                    cast_as,
                } => AggregateColumn {
                    func: AggFunc::Avg,
                    field: Some(field),
                    alias,
                    cast_as,
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
                AggregateColumnSpec::SumWhere {
                    field,
                    when,
                    alias,
                    cast_as,
                } => AggregateColumn {
                    cast_as,
                    ..AggregateColumn::sum_where(
                        alias,
                        field,
                        wafer_sql_utils::query::tree_to_simple_expr(&when),
                    )
                },
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

    /// Insert every row of `rows` into `collection` in one transaction and
    /// return the number inserted. Each row gets [`create`](Self::create)'s
    /// stamping; rows may carry different column sets. Either every row is
    /// stored or, when any insert fails, none is. No default: a backend that
    /// cannot make the inserts atomic must say so with an error. The database
    /// handler refuses a call carrying more than
    /// [`MAX_BATCH_WRITES`](wafer_block::wire::database::MAX_BATCH_WRITES)
    /// rows before it reaches this method.
    async fn create_many(
        &self,
        collection: &str,
        rows: Vec<HashMap<String, serde_json::Value>>,
    ) -> Result<i64, DatabaseError>;

    /// Apply `ops` in order as one transaction, returning one
    /// [`WriteOutcome`] per op in the same order. Either every op is applied
    /// or, when any statement fails, none is. An `Update`/`Delete` whose id
    /// matches no row is an outcome, not a failure. No default, for the same
    /// reason as [`create_many`](Self::create_many); the handler caps `ops` at
    /// [`MAX_BATCH_WRITES`](wafer_block::wire::database::MAX_BATCH_WRITES)
    /// the same way.
    async fn batch(&self, ops: Vec<WriteOp>) -> Result<Vec<WriteOutcome>, DatabaseError>;

    /// Insert `data` into `collection` only while every guard in `guards`
    /// holds over the table as it stands before the insert, returning the
    /// stored row, or which guard refused it. The row gets
    /// [`create`](Self::create)'s stamping. A key that is already taken is
    /// [`DatabaseError::AlreadyExists`].
    ///
    /// The check and the insert are ONE atomic step: no other guarded write
    /// to `collection` can land between them, so N concurrent inserts under a
    /// `CountBelow { cap }` leave at most `cap` rows. The refusal is computed
    /// in the same step, so the reported guard is the one that refused. A
    /// write through any other method is not serialised against it. No default: a backend must
    /// make the step atomic itself (the shared
    /// [`DbExec`](super::exec::DbExec) default renders one conditional
    /// statement and, on PostgreSQL, takes a per-table advisory lock first).
    async fn insert_guarded(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
        guards: &[CapGuard],
    ) -> Result<GuardedInsert, DatabaseError>;

    /// Set `data` on the rows of `collection` matching `filters` only while
    /// every guard in `guards` holds over the table as it stands before the
    /// update: [`GuardedUpdate::Updated`], [`GuardedUpdate::Refused`] naming
    /// the guard, or [`GuardedUpdate::NoMatch`] when no row matched (a
    /// missing table matches nothing, as
    /// [`update_where_count`](Self::update_where_count)). A guard that should
    /// not count a row the update replaces excludes it with a filter. Atomic
    /// as [`insert_guarded`](Self::insert_guarded) is; no default, for the
    /// same reason.
    async fn update_guarded(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
        guards: &[CapGuard],
    ) -> Result<GuardedUpdate, DatabaseError>;

    /// Update modifies an existing record by ID.
    async fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError>;

    /// Delete removes a record by ID.
    async fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError>;

    /// Count returns the number of records matching the filters; `0` for a
    /// table that does not exist.
    async fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError>;

    /// Sum returns the sum of a numeric field for matching records; `0` for a
    /// table that does not exist.
    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError>;

    /// QueryRaw executes a raw SELECT query.
    ///
    /// Raw SQL names no single source table, so JSON columns are not decoded
    /// the way the typed reads decode them, and the result differs by backend:
    /// on the SQLite family (native, D1, sql.js) a JSON column comes back as
    /// its stored JSON text — a string value as its quoted text, `"a"` — while
    /// Postgres returns `json`/`jsonb` columns structured. Read JSON columns
    /// through `get`/`list` for the same value everywhere, or parse the text.
    async fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError>;

    /// ExecRaw executes a raw non-SELECT statement.
    ///
    /// Values are bound as given: nothing is encoded for a JSON column the
    /// way the typed writes encode it (see
    /// [`codec`](super::codec)). To write a JSON column here, bind its JSON
    /// text — on Postgres a text parameter for a `json`/`jsonb` column must be
    /// JSON text.
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
                        limit: Some(10_000),
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

    /// Atomically select and delete all records matching filters, returning
    /// every deleted row.
    ///
    /// No default: a list-then-delete fallback is neither atomic nor able to
    /// reach every matching row in one read. The SQL backends render one
    /// `DELETE … WHERE … RETURNING *` statement through
    /// [`DbExec::take_where`](super::exec::DbExec::take_where).
    async fn take_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError>;

    /// Bulk-update all records matching filters in a single query.
    ///
    /// No default, for the same reason as [`take_where`](Self::take_where):
    /// the SQL backends render one `UPDATE … WHERE …` statement through
    /// [`DbExec::update_where`](super::exec::DbExec::update_where).
    async fn update_where(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError>;

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

    /// Grouped aggregate query; no groups for a table that does not exist.
    /// SQL backends implement this via `DbExec::aggregate`. No default —
    /// every backend states its choice.
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

    /// The column names of `table`, lowercased; empty when the table does
    /// not exist. No default: the database handler refuses an append-only
    /// insert naming a column this does not list, so a backend that cannot
    /// answer must say so with an error rather than guess.
    async fn schema_columns(&self, table: &str) -> Result<Vec<String>, DatabaseError>;

    /// Drop a table if it exists.
    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError>;

    /// Add a column to an existing table.
    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError>;

    /// Apply the resolved STRICT_SCHEMA flag
    /// (`WAFER_RUN__DATABASE__STRICT_SCHEMA`). Called once at lifecycle `Init`
    /// by the shared database handler, which reads the value from the node
    /// config on `ctx`.
    ///
    /// SQL backends store it so the shared executor
    /// ([`DbExec::strict_schema`](super::exec::DbExec::strict_schema)) can skip
    /// schema introspection on the hot path. The default is a no-op — backends
    /// and test mocks that don't cache a schema (or don't run through
    /// `DbExec`) simply ignore it.
    fn set_strict_schema(&self, _enabled: bool) {}
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
