//! Wire-format types for the database service.
//!
//! Mirrors `crates/wafer-core/src/interfaces/database/handler.rs` and
//! `crates/wafer-core/src/clients/database.rs`. `Record` and `RecordList`
//! match the runtime types in `interfaces::database::service`.
//!
//! Filter values and record fields are JSON-typed (`serde_json::Value`).
//! BLOB columns flow through as `serde_json::Value::Array` of integers
//! today — there is no dedicated `Vec<u8>` field on the wire — so this
//! module does not include a no-inflation test.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// --- Filter / sort sub-types ---

/// A single WHERE-clause predicate: `field <operator> value`, or
/// `field <operator> column` when `column` is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterDef {
    /// Column name to filter on.
    pub field: String,
    /// Comparison operator (`eq`, `ne`, `lt`, `gt`, `like`, …). Defaults to `eq`.
    #[serde(default = "default_operator")]
    pub operator: String,
    /// JSON value compared against the column.
    #[serde(default)]
    pub value: serde_json::Value,
    /// Another column of the same row to compare `field` against, in place
    /// of `value`.
    // Mutually exclusive with a non-null `value`, and only the six ordering
    // / equality operators apply; the handler rejects anything else as
    // `InvalidArgument`. Accepted wherever a filter *tree* is (`list`, and
    // the `CaseWhenSum` / `SumWhere` predicates of `aggregate`); the ops that
    // take flat filters (`count`, `sum`, the `*_where` family, and
    // `aggregate`'s own `filters`) reject it rather than drop it. Omitted
    // from the encoding when unset, so a request that does not use it
    // encodes exactly as it did before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
}

fn default_operator() -> String {
    "eq".to_string()
}

/// One node of a WHERE-clause predicate tree.
///
/// Serialized **untagged**: a leaf is the existing [`FilterDef`] object shape
/// (`field`/`operator`/`value`); a group is `{"all": [...]}` or
/// `{"any": [...]}`. The shapes are disjoint (a leaf always has `field`, a
/// group never does), so untagged matching is deterministic. A legacy flat
/// `[FilterDef]` array therefore decodes unchanged as `Vec<FilterNode::Leaf>`.
///
/// Depth and node-count bounds are enforced at conversion time in the
/// database handler, not here — deserialization stays a pure data step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FilterNode {
    /// A single comparison predicate.
    Leaf(FilterDef),
    /// AND of child predicates.
    All {
        /// Child predicates, all of which must hold.
        all: Vec<FilterNode>,
    },
    /// OR of child predicates.
    Any {
        /// Child predicates, at least one of which must hold.
        any: Vec<FilterNode>,
    },
}

/// One element of an ORDER BY clause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SortFieldDef {
    /// Column name to sort by.
    pub field: String,
    /// Whether to sort descending (default `false` = ascending).
    #[serde(default)]
    pub desc: bool,
}

// --- Requests ---

/// Request for `database.get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetRequest {
    /// Collection (table) name.
    pub collection: String,
    /// Primary-key id of the row.
    pub id: String,
}

/// Request for `database.list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRequest {
    /// Collection (table) name.
    pub collection: String,
    /// WHERE-clause predicates (AND-combined).
    #[serde(default)]
    pub filters: Vec<FilterNode>,
    /// ORDER BY clause.
    #[serde(default)]
    pub sort: Vec<SortFieldDef>,
    /// Maximum number of rows to return, at least 1; absent returns every
    /// matching row.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Number of rows to skip for pagination; a positive offset needs a
    /// `limit`.
    #[serde(default)]
    pub offset: i64,
    /// When `true`, backends skip the `SELECT COUNT(*)` query and return
    /// `RecordList.total_count = records.len() as i64` (count of records
    /// returned this call, not total matching in the collection). Used by
    /// `wafer-core::clients::database::{list_all, list_sorted}`. Paginated
    /// UIs should leave this `false` and read `total_count` normally.
    #[serde(default)]
    pub skip_count: bool,
    /// Optional column projection. `None` (the default) selects every
    /// column; `Some(cols)` selects exactly `cols`. An empty `Some(vec![])`
    /// is rejected by the handler as `InvalidArgument` — it can't express
    /// "no columns" as a meaningful SELECT.
    #[serde(default)]
    pub columns: Option<Vec<String>>,
}

/// Request for `database.create`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRequest {
    /// Collection (table) name.
    pub collection: String,
    /// Column → value map to insert.
    pub data: HashMap<String, serde_json::Value>,
}

/// Request for `database.update`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRequest {
    /// Collection (table) name.
    pub collection: String,
    /// Primary-key id of the row to update.
    pub id: String,
    /// Column → value map to set.
    pub data: HashMap<String, serde_json::Value>,
}

/// Request for `database.delete`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteRequest {
    /// Collection (table) name.
    pub collection: String,
    /// Primary-key id of the row to delete.
    pub id: String,
}

/// Request for `database.count`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CountRequest {
    /// Collection (table) name.
    pub collection: String,
    /// WHERE-clause predicates (AND-combined).
    #[serde(default)]
    pub filters: Vec<FilterNode>,
}

/// Request for `database.sum`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SumRequest {
    /// Collection (table) name.
    pub collection: String,
    /// Numeric column to sum.
    pub field: String,
    /// WHERE-clause predicates (AND-combined).
    #[serde(default)]
    pub filters: Vec<FilterNode>,
}

/// Request for `database.query_raw`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRawRequest {
    /// SQL `SELECT` text with `?` placeholders.
    pub query: String,
    /// Positional bind arguments for the placeholders.
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

/// Request for `database.exec_raw`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRawRequest {
    /// SQL mutation statement (`INSERT`, `UPDATE`, `DELETE`, DDL).
    pub query: String,
    /// Positional bind arguments for the placeholders.
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

/// One column of a [`TableDef`] / [`AddColumnRequest`].
///
/// `kind` is one of `string`, `text`, `int`, `int64`, `float`, `bool`,
/// `datetime`, `json`, `blob` — the names of `wafer_schema::DataType`,
/// lower-cased. The host maps them; an unknown kind is `InvalidArgument`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnDef {
    /// Column name.
    pub name: String,
    /// Data type: `string`, `text`, `int`, `int64`, `float`, `bool`,
    /// `datetime`, `json`, or `blob`.
    pub kind: String,
    /// Whether the column allows `NULL`.
    #[serde(default)]
    pub nullable: bool,
    /// Whether this column is (part of) the table's primary key.
    #[serde(default)]
    pub primary_key: bool,
    /// Whether this column auto-increments (integer primary keys only).
    #[serde(default)]
    pub auto_increment: bool,
    /// Whether this column carries a `UNIQUE` constraint.
    #[serde(default)]
    pub unique: bool,
    /// Default value applied when the column is omitted on insert.
    #[serde(default)]
    pub default: Option<DefaultDef>,
}

/// A column default. `kind` is `null`, `now`, or `value` (with `value` a
/// JSON string, integer, float or boolean). There is deliberately no raw
/// SQL kind: a schema op never carries a SQL fragment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultDef {
    /// Default kind: `null`, `now`, or `value`.
    pub kind: String,
    /// The literal default value when `kind` is `value`; ignored otherwise.
    #[serde(default)]
    pub value: serde_json::Value,
}

/// A secondary index of a [`TableDef`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDef {
    /// Index name. Empty lets the host derive one from `columns`.
    #[serde(default)]
    pub name: String,
    /// Indexed columns, in order.
    pub columns: Vec<String>,
    /// Whether the index enforces uniqueness.
    #[serde(default)]
    pub unique: bool,
}

/// A table definition for `database.ensure_table`. Mirrors
/// `wafer_schema::Table` field for field; the host converts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableDef {
    /// Table name.
    pub name: String,
    /// Column definitions.
    pub columns: Vec<ColumnDef>,
    /// Secondary indexes to create alongside the table.
    #[serde(default)]
    pub indexes: Vec<IndexDef>,
    /// Composite primary-key columns, when the primary key spans more than
    /// one column (single-column primary keys are declared on the column
    /// itself via `ColumnDef::primary_key`).
    #[serde(default)]
    pub primary_key: Vec<String>,
    /// Composite `UNIQUE` constraints, each a set of columns.
    #[serde(default)]
    pub unique_keys: Vec<Vec<String>>,
}

/// Request for `database.ensure_table` — create the table and its indexes
/// if they do not exist. Authorized on `table.name` and on `__schema__`
/// (the `schema` capability); it does NOT require raw `__ddl__`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsureTableRequest {
    /// The table to ensure exists.
    pub table: TableDef,
}

/// Request for `database.add_column`. Authorized on `table` and `__schema__`
/// (the `schema` capability); it does NOT require raw `__ddl__`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddColumnRequest {
    /// Table (collection) name.
    pub table: String,
    /// Column to add.
    pub column: ColumnDef,
}

/// Request for `database.drop_table`. Authorized on `table` and `__schema__`
/// (the `schema` capability); it does NOT require raw `__ddl__`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropTableRequest {
    /// Table (collection) name.
    pub table: String,
}

/// Request for `database.table_exists` — a read, authorized on `table` only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableExistsRequest {
    /// Table (collection) name.
    pub table: String,
}

/// Response for `ensure_table`, `add_column` and `drop_table`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaOpResponse {
    /// Table (collection) name the op was applied to.
    pub table: String,
}

/// Response for `database.table_exists`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableExistsResponse {
    /// Table (collection) name that was checked.
    pub table: String,
    /// Whether the table exists.
    pub exists: bool,
}

/// Request for `database.delete_where`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteWhereRequest {
    /// Collection (table) name.
    pub collection: String,
    /// WHERE-clause predicates (AND-combined).
    #[serde(default)]
    pub filters: Vec<FilterNode>,
}

/// Request for `database.delete_where_count`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteWhereCountRequest {
    /// Collection (table) name.
    pub collection: String,
    /// WHERE-clause predicates (AND-combined).
    #[serde(default)]
    pub filters: Vec<FilterNode>,
}

/// Request for `database.take_where`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakeWhereRequest {
    /// Collection (table) name.
    pub collection: String,
    /// WHERE-clause predicates (AND-combined).
    #[serde(default)]
    pub filters: Vec<FilterNode>,
}

/// Request for `database.update_where`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateWhereRequest {
    /// Collection (table) name.
    pub collection: String,
    /// WHERE-clause predicates (AND-combined).
    #[serde(default)]
    pub filters: Vec<FilterNode>,
    /// Column → value map to set on matching rows.
    pub data: HashMap<String, serde_json::Value>,
}

/// Request for `database.update_where_count`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateWhereCountRequest {
    /// Collection (table) name.
    pub collection: String,
    /// WHERE-clause predicates (AND-combined).
    #[serde(default)]
    pub filters: Vec<FilterNode>,
    /// Column → value map to set on matching rows.
    pub data: HashMap<String, serde_json::Value>,
}

/// Request for `database.increment_field_where`. Atomically increments a
/// numeric column on every row matching the filter — a single
/// `UPDATE … SET col = col + delta WHERE …` round-trip with no
/// read-modify-write race.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncrementFieldWhereRequest {
    /// Collection (table) name.
    pub collection: String,
    /// Column to atomically increment.
    pub col: String,
    /// Signed delta to add (negative = decrement).
    pub delta: i64,
    /// WHERE-clause predicates (AND-combined).
    #[serde(default)]
    pub filters: Vec<FilterNode>,
}

/// Request for `database.upsert`. Insert `data`, resolving a conflict on
/// `conflict_columns` via `on_conflict`, as a single atomic
/// `INSERT … ON CONFLICT …`. The handler renders the SQL server-side against
/// the WRAP-authorized `collection`, so the table run always *is* the
/// collection that was checked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertRequest {
    /// Collection (table) name. WRAP-authorized (write).
    pub collection: String,
    /// Insert column → value pairs. Order is preserved so the generated
    /// INSERT is deterministic across process starts.
    pub data: Vec<(String, serde_json::Value)>,
    /// Conflict-target columns (must carry a `UNIQUE`/`PRIMARY KEY` constraint).
    pub conflict_columns: Vec<String>,
    /// What to do when the insert conflicts on `conflict_columns`.
    pub on_conflict: OnConflict,
}

/// Most ops one `database.batch` call, or rows one `database.create_many`
/// call, may carry; the database handler answers a larger call with
/// `InvalidArgument` before anything runs.
///
/// Every op or row is one SQL statement, so this is sized to Cloudflare's
/// per-Worker-invocation D1 query limit on Workers Paid (1000; the Free plan
/// allows 50, so a Free-plan consumer chunks smaller): a call that fits here
/// fits one invocation. It also bounds how long one call holds a backend's
/// write path inside its transaction (SQLite has a single write connection).
pub const MAX_BATCH_WRITES: usize = 1000;

/// Request for `database.create_many`: insert every row of `rows` into
/// `collection` in one transaction — all of them or, when any insert fails,
/// none. Rows may carry different column sets. WRAP-authorized (append)
/// against `collection`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateManyRequest {
    /// Collection (table) name.
    pub collection: String,
    /// One column → value map per row; `id` and timestamps are stamped when
    /// absent, as for `database.create`. At most [`MAX_BATCH_WRITES`].
    pub rows: Vec<HashMap<String, serde_json::Value>>,
}

/// Request for `database.batch`: apply `ops` in order as one transaction —
/// all of them or, when any statement fails, none. Every op is
/// WRAP-authorized on its collection, with the access
/// [`BatchWrite::access`] names, before anything runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequest {
    /// The writes, applied in order. At most [`MAX_BATCH_WRITES`].
    pub ops: Vec<BatchWrite>,
}

/// One write in a [`BatchRequest`], with the semantics of the single op it is
/// named after — except that an `Update` or `Delete` whose `id` matches no
/// row is reported in its [`BatchWriteResult`] instead of failing the batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BatchWrite {
    /// As `database.create`.
    Create {
        /// Collection (table) name.
        collection: String,
        /// Column → value map.
        data: HashMap<String, serde_json::Value>,
    },
    /// As `database.update`.
    Update {
        /// Collection (table) name.
        collection: String,
        /// Primary-key id of the row.
        id: String,
        /// Column → value map to set.
        data: HashMap<String, serde_json::Value>,
    },
    /// As `database.delete`.
    Delete {
        /// Collection (table) name.
        collection: String,
        /// Primary-key id of the row.
        id: String,
    },
    /// As `database.update_where_count`.
    UpdateWhere {
        /// Collection (table) name.
        collection: String,
        /// WHERE-clause predicates (AND-combined leaves).
        #[serde(default)]
        filters: Vec<FilterNode>,
        /// Column → value map to set on matching rows.
        data: HashMap<String, serde_json::Value>,
    },
    /// As `database.upsert`.
    Upsert(UpsertRequest),
}

impl BatchWrite {
    /// The collection this write targets — the resource it is authorized
    /// against.
    #[must_use]
    pub fn collection(&self) -> &str {
        match self {
            Self::Create { collection, .. }
            | Self::Update { collection, .. }
            | Self::Delete { collection, .. }
            | Self::UpdateWhere { collection, .. } => collection,
            Self::Upsert(req) => &req.collection,
        }
    }

    /// The access this write needs on its collection: `Create` only
    /// inserts, so it is an append; every other variant changes existing
    /// rows.
    #[must_use]
    pub fn access(&self) -> crate::types::ResourceAccess {
        match self {
            Self::Create { .. } => crate::types::ResourceAccess::Append,
            Self::Update { .. }
            | Self::Delete { .. }
            | Self::UpdateWhere { .. }
            | Self::Upsert(_) => crate::types::ResourceAccess::Write,
        }
    }
}

/// Most cap guards one `database.insert_guarded` or `database.update_guarded`
/// call may carry; the database handler answers a larger call with
/// `InvalidArgument` before anything runs. Each guard is one aggregate
/// subquery in the write's statement.
pub const MAX_WRITE_GUARDS: usize = 16;

/// A cap a guarded write must stay within, measured over the rows of the
/// written collection that match `filters` as they stand BEFORE the write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CapGuard {
    /// Holds when fewer than `cap` rows match `filters`.
    CountBelow {
        /// WHERE-clause predicates (AND-combined leaves).
        #[serde(default)]
        filters: Vec<FilterNode>,
        /// Largest row count an insert may leave.
        cap: i64,
    },
    /// Holds when `SUM(field)` over the rows matching `filters`, plus `add`,
    /// is at most `cap`.
    SumAtMost {
        /// Numeric column summed.
        field: String,
        /// WHERE-clause predicates (AND-combined leaves).
        #[serde(default)]
        filters: Vec<FilterNode>,
        /// What the write adds to the sum.
        add: i64,
        /// Largest total the write may leave.
        cap: i64,
    },
}

/// Request for `database.insert_guarded`: insert `data` into `collection`
/// only while every guard holds. WRAP-authorized (append and read — the
/// guards measure existing rows) against `collection`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsertGuardedRequest {
    /// Collection (table) name.
    pub collection: String,
    /// Column → value map; `id` and timestamps are stamped when absent, as
    /// for `database.create`.
    pub data: HashMap<String, serde_json::Value>,
    /// Caps that must all hold. At most [`MAX_WRITE_GUARDS`].
    pub guards: Vec<CapGuard>,
}

/// Request for `database.update_guarded`: set `data` on the rows of
/// `collection` matching `filters` only while every guard holds.
/// WRAP-authorized (write) against `collection`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateGuardedRequest {
    /// Collection (table) name.
    pub collection: String,
    /// WHERE-clause predicates selecting the updated rows (AND-combined
    /// leaves).
    #[serde(default)]
    pub filters: Vec<FilterNode>,
    /// Column → value map to set on matching rows.
    pub data: HashMap<String, serde_json::Value>,
    /// Caps that must all hold. At most [`MAX_WRITE_GUARDS`].
    pub guards: Vec<CapGuard>,
}

/// Conflict-resolution strategy for [`UpsertRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OnConflict {
    /// `ON CONFLICT (conflict_columns) DO UPDATE SET <cols> = excluded.<cols>`.
    /// An empty column list degrades to `DO NOTHING` (insert-or-ignore).
    SetColumns(Vec<String>),
    /// Atomic sliding-window counter (the fixed-window rate-limit pattern).
    ///
    /// On insert the server seeds `count_field = 1` and `window_field = now`;
    /// on conflict, `count_field` resets to 1 when the stored `window_field`
    /// is strictly older than `window_cutoff` (also rolling `window_field`
    /// forward to `now`), otherwise increments by 1. The `id` and `key`
    /// insert values are read from `data` (a fresh row identifier and the
    /// conflict-target value).
    WindowedCounter {
        /// Counter column (e.g. `count`).
        count_field: String,
        /// Window-start column (e.g. `window_start`).
        window_field: String,
        /// Current epoch-seconds, recorded as `window_field` on insert/reset.
        now: i64,
        /// `now - window_secs`; rows whose stored `window_field` is strictly
        /// less than this are treated as expired and reset.
        window_cutoff: i64,
        /// Creation-timestamp columns, stamped `CURRENT_TIMESTAMP` on INSERT
        /// **only** — never re-written on conflict, so creation time is
        /// immutable across counter updates.
        created_fields: Vec<String>,
        /// Modification-timestamp columns, stamped `CURRENT_TIMESTAMP` on both
        /// the initial INSERT and every conflicting update.
        updated_fields: Vec<String>,
    },
}

/// Request for `database.aggregate` (grouped aggregate read). The handler
/// renders the SQL server-side from this structured request against the
/// WRAP-authorized `collection`, so — unlike `query_raw` — no raw SQL crosses
/// the boundary and the statement always targets the checked collection.
/// The response is a `Vec<Record>`, one per group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateRequest {
    /// Collection (table) name. WRAP-authorized (read).
    pub collection: String,
    /// Plain (non-aggregated) columns to also select — typically the same
    /// columns named in `group_by`. Empty for pure aggregates.
    #[serde(default)]
    pub select_columns: Vec<String>,
    /// Aggregate output columns. At least one is required — the handler
    /// rejects an empty list as `InvalidArgument`.
    pub aggregates: Vec<AggregateColumnDef>,
    /// WHERE-clause predicates. AND-combined leaves only; a group node is
    /// rejected as `InvalidArgument` (consistent with `count`/`sum`).
    #[serde(default)]
    pub filters: Vec<FilterNode>,
    /// GROUP BY terms — plain columns and/or date buckets.
    #[serde(default)]
    pub group_by: Vec<GroupByDef>,
    /// ORDER BY clause. Aggregate aliases are valid sort keys.
    #[serde(default)]
    pub sort: Vec<SortFieldDef>,
    /// Optional `LIMIT N`; a value `<= 0` means no limit.
    #[serde(default)]
    pub limit: i64,
}

/// One aggregate output column for [`AggregateRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AggregateColumnDef {
    /// `COUNT(*) AS alias`.
    Count {
        /// Output alias for the count.
        alias: String,
    },
    /// `SUM(field) AS alias`, or `CAST(SUM(field) AS <cast_as>) AS alias`.
    Sum {
        /// Numeric column to sum.
        field: String,
        /// Output alias for the sum.
        alias: String,
        /// Optional output cast: `BIGINT` or `DOUBLE PRECISION`.
        // Validated against that allowlist by the handler (anything else is
        // `InvalidArgument`). Postgres widens `SUM(<bigint>)` to `NUMERIC`,
        // which decodes as a float; `BIGINT` pins an integral sum to an
        // integer on every backend. A non-integral value is rounded by the
        // `BIGINT` cast on Postgres and truncated on SQLite, so cast to
        // `BIGINT` only a sum of integers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cast_as: Option<String>,
    },
    /// `AVG(field) AS alias`, or `CAST(AVG(field) AS DOUBLE PRECISION) AS
    /// alias`.
    Avg {
        /// Numeric column to average.
        field: String,
        /// Output alias for the average.
        alias: String,
        /// Optional output cast: `DOUBLE PRECISION` only.
        // The handler rejects `BIGINT` here as `InvalidArgument`: an average
        // is rarely integral, and the cast rounds it on Postgres but
        // truncates it on SQLite, so one request would answer differently
        // per backend.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cast_as: Option<String>,
    },
    /// `MAX(field) AS alias` — greatest value in each group.
    Max {
        /// Column to take the maximum of.
        field: String,
        /// Output alias for the maximum.
        alias: String,
    },
    /// `COALESCE(SUM(CASE WHEN <when> THEN 1 ELSE 0 END), 0) AS alias` — a
    /// portable conditional count (no `FILTER` clause required), `0` when no
    /// row matches. `when` is a predicate forest, AND-combined at the top
    /// level; the handler bounds and validates it, and the server builds the
    /// `CASE` predicate (the sea-query expression is `!Send`, so it can't be
    /// built caller-side). An empty `when` is rejected as `InvalidArgument`.
    CaseWhenSum {
        /// Predicate whose matching rows are counted.
        when: Vec<FilterNode>,
        /// Output alias for the conditional count.
        alias: String,
    },
    /// `COALESCE(SUM(CASE WHEN <when> THEN field ELSE 0 END), 0) AS alias` —
    /// the sum of `field` over the rows matching `when`, `0` when nothing
    /// non-null is summed; optionally cast like `Sum`.
    // `when` is bounded and validated exactly like `CaseWhenSum.when`, and an
    // empty `when` is rejected as `InvalidArgument`. When no row matches,
    // SQLite's `0` is the integer `0` even over a `REAL` column (Postgres
    // gives the column's type), so it decodes as a JSON integer there;
    // `cast_as: "DOUBLE PRECISION"` reads a float on every backend.
    SumWhere {
        /// Numeric column to sum over the matching rows.
        field: String,
        /// Predicate selecting the rows whose `field` is summed.
        when: Vec<FilterNode>,
        /// Output alias for the conditional sum.
        alias: String,
        /// Optional output cast: `BIGINT` or `DOUBLE PRECISION`.
        // Same allowlist, and the same rounding-versus-truncation caveat for
        // `BIGINT`, as `Sum::cast_as`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cast_as: Option<String>,
    },
}

/// One GROUP BY term for [`AggregateRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GroupByDef {
    /// Group by a plain column.
    Column(String),
    /// Group by the date bucket `date(field)` — the day portion of a
    /// timestamp column. The bucketed value is emitted in each result row
    /// under the `field` name.
    DateBucket {
        /// Timestamp column to bucket by day.
        field: String,
    },
}

// --- Responses ---

/// Single record returned by `get`, `create`, `update`. Matches
/// `interfaces::database::service::Record`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    /// Primary-key id of the row.
    pub id: String,
    /// Column → value map.
    pub data: HashMap<String, serde_json::Value>,
}

/// Paginated list of records returned by `list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordList {
    /// Records in this page.
    pub records: Vec<Record>,
    /// Total matching rows in the collection (or this page's count when
    /// `skip_count` was set on the request).
    pub total_count: i64,
    /// 1-indexed page number.
    pub page: i64,
    /// Number of records per page.
    pub page_size: i64,
}

/// Response for `database.count`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CountResponse {
    /// Number of matching rows.
    pub count: i64,
}

/// Response for `database.sum`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SumResponse {
    /// Aggregated sum of the requested column.
    pub sum: f64,
}

/// Response for `database.delete_where_count`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteWhereCountResponse {
    /// Number of rows deleted.
    pub count: i64,
}

/// Response for `database.update_where_count`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateWhereCountResponse {
    /// Number of rows updated.
    pub count: i64,
}

/// Response for `database.take_where`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakeWhereResponse {
    /// Rows that were atomically removed and returned.
    pub records: Vec<Record>,
}

/// Response for `database.exec_raw`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRawResponse {
    /// Number of rows affected by the statement.
    pub rows_affected: i64,
}

/// Response for `database.create_many`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateManyResponse {
    /// Number of rows inserted.
    pub rows_affected: i64,
}

/// Response for `database.batch`: one result per op, in the order of
/// [`BatchRequest::ops`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResponse {
    /// Per-op results.
    pub results: Vec<BatchWriteResult>,
}

/// The result of one [`BatchWrite`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BatchWriteResult {
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

/// Response for `database.insert_guarded`. A key that is already taken is
/// not a response but an `AlreadyExists` error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InsertGuardedResponse {
    /// Every guard held; the row as stored.
    Inserted {
        /// The inserted row, including its id.
        record: Record,
    },
    /// The guard at this index of the request's `guards` refused the insert
    /// (the first that did); nothing was written.
    Refused {
        /// Index into the request's `guards`.
        guard: usize,
    },
}

/// Response for `database.update_guarded`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UpdateGuardedResponse {
    /// Every guard held; this many rows (at least one) matched and were
    /// updated.
    Updated {
        /// Rows updated.
        rows_affected: i64,
    },
    /// The guard at this index of the request's `guards` refused the update
    /// (the first that did); nothing was written. Guards are checked before
    /// the filters.
    Refused {
        /// Index into the request's `guards`.
        guard: usize,
    },
    /// Every guard held but no row matched the filters; nothing was written.
    NoMatch,
}

/// Response for `database.upsert`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertResponse {
    /// Rows affected by the insert/update.
    pub rows_affected: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    // -----------------------------------------------------------------------
    // Round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn get_request_round_trips() {
        let original = GetRequest {
            collection: "users".into(),
            id: "u1".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: GetRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.collection, original.collection);
        assert_eq!(decoded.id, original.id);
    }

    #[test]
    fn list_request_round_trips() {
        let original = ListRequest {
            collection: "users".into(),
            filters: vec![FilterNode::Leaf(FilterDef {
                field: "active".into(),
                operator: "eq".into(),
                value: serde_json::json!(true),
                column: None,
            })],
            sort: vec![SortFieldDef {
                field: "created_at".into(),
                desc: true,
            }],
            limit: Some(50),
            offset: 100,
            skip_count: false,
            columns: Some(vec!["id".into(), "active".into()]),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ListRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.collection, original.collection);
        assert_eq!(decoded.limit, Some(50));
        assert_eq!(decoded.offset, 100);
        assert_eq!(decoded.filters.len(), 1);
        assert_eq!(decoded.sort.len(), 1);
        assert!(decoded.sort[0].desc);
        assert!(!decoded.skip_count);
        assert_eq!(
            decoded.columns,
            Some(vec!["id".to_string(), "active".to_string()])
        );
    }

    #[test]
    fn list_request_columns_none_round_trips() {
        let original = ListRequest {
            collection: "users".into(),
            filters: vec![],
            sort: vec![],
            limit: None,
            offset: 0,
            skip_count: false,
            columns: None,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ListRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.columns, None);
    }

    #[test]
    fn create_request_round_trips() {
        let mut data = HashMap::new();
        data.insert("name".into(), serde_json::json!("Alice"));
        let original = CreateRequest {
            collection: "users".into(),
            data,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: CreateRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.collection, original.collection);
        assert_eq!(decoded.data.get("name"), Some(&serde_json::json!("Alice")));
    }

    #[test]
    fn record_round_trips() {
        let mut data = HashMap::new();
        data.insert("k".into(), serde_json::json!("v"));
        let original = Record {
            id: "r1".into(),
            data,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: Record = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.id, original.id);
        assert_eq!(decoded.data.get("k"), Some(&serde_json::json!("v")));
    }

    #[test]
    fn record_list_round_trips() {
        let original = RecordList {
            records: vec![Record {
                id: "r1".into(),
                data: HashMap::new(),
            }],
            total_count: 1,
            page: 1,
            page_size: 20,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: RecordList = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.records.len(), 1);
        assert_eq!(decoded.total_count, 1);
        assert_eq!(decoded.page, 1);
        assert_eq!(decoded.page_size, 20);
    }

    #[test]
    fn upsert_request_set_columns_round_trips() {
        let original = UpsertRequest {
            collection: "widgets".into(),
            data: vec![
                ("id".into(), serde_json::json!("w1")),
                ("name".into(), serde_json::json!("gizmo")),
            ],
            conflict_columns: vec!["id".into()],
            on_conflict: OnConflict::SetColumns(vec!["name".into()]),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: UpsertRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.collection, "widgets");
        assert_eq!(decoded.data.len(), 2);
        assert_eq!(decoded.conflict_columns, vec!["id".to_string()]);
        match decoded.on_conflict {
            OnConflict::SetColumns(cols) => assert_eq!(cols, vec!["name".to_string()]),
            other => panic!("expected SetColumns, got {other:?}"),
        }
    }

    #[test]
    fn upsert_request_windowed_counter_round_trips() {
        let original = UpsertRequest {
            collection: "rate_limits".into(),
            data: vec![
                ("id".into(), serde_json::json!("rl-1")),
                ("key".into(), serde_json::json!("user:1:login")),
            ],
            conflict_columns: vec!["key".into()],
            on_conflict: OnConflict::WindowedCounter {
                count_field: "count".into(),
                window_field: "window_start".into(),
                now: 1_700_000_000,
                window_cutoff: 1_699_999_940,
                created_fields: vec!["created_at".into()],
                updated_fields: vec!["updated_at".into()],
            },
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: UpsertRequest = codec::decode(&encoded).expect("decode");
        match decoded.on_conflict {
            OnConflict::WindowedCounter {
                count_field,
                window_field,
                now,
                window_cutoff,
                created_fields,
                updated_fields,
            } => {
                assert_eq!(count_field, "count");
                assert_eq!(window_field, "window_start");
                assert_eq!(now, 1_700_000_000);
                assert_eq!(window_cutoff, 1_699_999_940);
                assert_eq!(created_fields, vec!["created_at".to_string()]);
                assert_eq!(updated_fields, vec!["updated_at".to_string()]);
            }
            other => panic!("expected WindowedCounter, got {other:?}"),
        }
    }

    #[test]
    fn aggregate_request_round_trips() {
        let original = AggregateRequest {
            collection: "request_logs".into(),
            select_columns: vec!["method".into()],
            aggregates: vec![
                AggregateColumnDef::Count {
                    alias: "cnt".into(),
                },
                AggregateColumnDef::Sum {
                    field: "bytes".into(),
                    alias: "total_bytes".into(),
                    cast_as: None,
                },
                AggregateColumnDef::Avg {
                    field: "duration_ms".into(),
                    alias: "avg_ms".into(),
                    cast_as: None,
                },
                AggregateColumnDef::CaseWhenSum {
                    when: vec![FilterNode::Leaf(FilterDef {
                        field: "status".into(),
                        operator: "gte".into(),
                        value: serde_json::json!(400),
                        column: None,
                    })],
                    alias: "errors".into(),
                },
            ],
            filters: vec![FilterNode::Leaf(FilterDef {
                field: "active".into(),
                operator: "eq".into(),
                value: serde_json::json!(true),
                column: None,
            })],
            group_by: vec![
                GroupByDef::Column("method".into()),
                GroupByDef::DateBucket {
                    field: "created_at".into(),
                },
            ],
            sort: vec![SortFieldDef {
                field: "cnt".into(),
                desc: true,
            }],
            limit: 50,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: AggregateRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.collection, "request_logs");
        assert_eq!(decoded.select_columns, vec!["method".to_string()]);
        assert_eq!(decoded.aggregates.len(), 4);
        assert_eq!(decoded.filters.len(), 1);
        assert_eq!(decoded.group_by.len(), 2);
        assert_eq!(decoded.limit, 50);
        assert!(decoded.sort[0].desc);
        match &decoded.aggregates[3] {
            AggregateColumnDef::CaseWhenSum { when, alias } => {
                assert_eq!(alias, "errors");
                assert_eq!(when.len(), 1);
            }
            other => panic!("expected CaseWhenSum, got {other:?}"),
        }
        match &decoded.group_by[1] {
            GroupByDef::DateBucket { field } => assert_eq!(field, "created_at"),
            other => panic!("expected DateBucket, got {other:?}"),
        }
    }

    /// `filters`, `group_by`, `sort`, `select_columns`, and `limit` all carry
    /// `#[serde(default)]`, so a minimal request that only names a collection
    /// and one aggregate must decode with those fields defaulted/empty.
    #[test]
    fn aggregate_request_minimal_defaults_round_trip() {
        let original = AggregateRequest {
            collection: "t".into(),
            select_columns: vec![],
            aggregates: vec![AggregateColumnDef::Count {
                alias: "cnt".into(),
            }],
            filters: vec![],
            group_by: vec![],
            sort: vec![],
            limit: 0,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: AggregateRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.collection, "t");
        assert_eq!(decoded.aggregates.len(), 1);
        assert!(decoded.filters.is_empty());
        assert!(decoded.group_by.is_empty());
        assert_eq!(decoded.limit, 0);
    }

    #[test]
    fn query_raw_request_round_trips() {
        let original = QueryRawRequest {
            query: "SELECT 1".into(),
            args: vec![serde_json::json!(1), serde_json::json!("x")],
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: QueryRawRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.query, original.query);
        assert_eq!(decoded.args.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Schema-lock tests
    // -----------------------------------------------------------------------

    #[test]
    fn schema_lock_get_request() {
        let req = GetRequest {
            collection: String::new(),
            id: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82aa636f6c6c656374696f6ea0a26964a0",
            "GetRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_list_request() {
        let req = ListRequest {
            collection: String::new(),
            filters: vec![],
            sort: vec![],
            limit: None,
            offset: 0,
            skip_count: false,
            columns: None,
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "87aa636f6c6c656374696f6ea0a766696c7465727390a4736f727490a56c696d6974c0a66f666673657400aa736b69705f636f756e74c2a7636f6c756d6e73c0",
            "ListRequest schema changed — review consumer impact before updating this literal"
        );
    }

    /// Forward-compat: an old encoder that omits `skip_count` must still
    /// decode into the new `ListRequest`, defaulting `skip_count` to `false`.
    /// The legacy hex below is the pre-skip_count `ListRequest` encoding
    /// (captured before this field was added).
    #[test]
    fn list_request_decodes_with_missing_skip_count() {
        let legacy_hex =
            "85aa636f6c6c656374696f6ea0a766696c7465727390a4736f727490a56c696d697400a66f666673657400";
        let bytes: Vec<u8> = (0..legacy_hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&legacy_hex[i..i + 2], 16).unwrap())
            .collect();
        let decoded: ListRequest = codec::decode(&bytes).expect("decode legacy");
        assert!(!decoded.skip_count);
        assert_eq!(decoded.collection, "");
        // An old encoder always wrote `limit: 0` for "no limit". It decodes
        // as `Some(0)`, which the select builder refuses (`ZeroLimit`), so an
        // old guest's list fails loudly instead of returning no rows.
        assert_eq!(decoded.limit, Some(0));
    }

    #[test]
    fn schema_lock_record() {
        let r = Record {
            id: String::new(),
            data: HashMap::new(),
        };
        let encoded = codec::encode(&r).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82a26964a0a46461746180",
            "Record schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_record_list() {
        let rl = RecordList {
            records: vec![],
            total_count: 0,
            page: 0,
            page_size: 0,
        };
        let encoded = codec::encode(&rl).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "84a77265636f72647390ab746f74616c5f636f756e7400a47061676500a9706167655f73697a6500",
            "RecordList schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_count_response() {
        let r = CountResponse { count: 0 };
        let encoded = codec::encode(&r).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a5636f756e7400",
            "CountResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_exec_raw_response() {
        let r = ExecRawResponse { rows_affected: 0 };
        let encoded = codec::encode(&r).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81ad726f77735f616666656374656400",
            "ExecRawResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn create_many_request_round_trips() {
        let original = CreateManyRequest {
            collection: "items".into(),
            rows: vec![
                HashMap::from([("name".to_string(), serde_json::json!("a"))]),
                HashMap::from([("other".to_string(), serde_json::json!(2))]),
            ],
        };
        let decoded: CreateManyRequest =
            codec::decode(&codec::encode(&original).expect("encode")).expect("decode");
        assert_eq!(decoded.collection, "items");
        assert_eq!(decoded.rows, original.rows);
    }

    #[test]
    fn batch_request_round_trips_every_write() {
        let original = BatchRequest {
            ops: vec![
                BatchWrite::Create {
                    collection: "a".into(),
                    data: HashMap::from([("k".to_string(), serde_json::json!(1))]),
                },
                BatchWrite::Update {
                    collection: "b".into(),
                    id: "1".into(),
                    data: HashMap::new(),
                },
                BatchWrite::Delete {
                    collection: "c".into(),
                    id: "2".into(),
                },
                BatchWrite::UpdateWhere {
                    collection: "d".into(),
                    filters: vec![FilterNode::Leaf(FilterDef {
                        field: "k".into(),
                        operator: "eq".into(),
                        value: serde_json::json!(1),
                        column: None,
                    })],
                    data: HashMap::new(),
                },
                BatchWrite::Upsert(UpsertRequest {
                    collection: "e".into(),
                    data: vec![("id".into(), serde_json::json!("3"))],
                    conflict_columns: vec!["id".into()],
                    on_conflict: OnConflict::SetColumns(Vec::new()),
                }),
            ],
        };
        let decoded: BatchRequest =
            codec::decode(&codec::encode(&original).expect("encode")).expect("decode");
        let collections: Vec<&str> = decoded.ops.iter().map(BatchWrite::collection).collect();
        assert_eq!(collections, ["a", "b", "c", "d", "e"]);
        assert!(matches!(&decoded.ops[1], BatchWrite::Update { id, .. } if id == "1"));
        assert!(
            matches!(&decoded.ops[3], BatchWrite::UpdateWhere { filters, .. } if filters.len() == 1)
        );
    }

    #[test]
    fn batch_response_round_trips_every_result() {
        let record = Record {
            id: "1".into(),
            data: HashMap::new(),
        };
        let original = BatchResponse {
            results: vec![
                BatchWriteResult::Created(record.clone()),
                BatchWriteResult::Updated(Some(record)),
                BatchWriteResult::Updated(None),
                BatchWriteResult::Deleted { rows_affected: 0 },
                BatchWriteResult::UpdatedWhere { rows_affected: 2 },
                BatchWriteResult::Upserted { rows_affected: 1 },
            ],
        };
        let decoded: BatchResponse =
            codec::decode(&codec::encode(&original).expect("encode")).expect("decode");
        assert_eq!(format!("{decoded:?}"), format!("{original:?}"));
    }

    #[test]
    fn guarded_write_requests_round_trip_every_guard() {
        let guards = vec![
            CapGuard::CountBelow {
                filters: vec![FilterNode::Leaf(FilterDef {
                    field: "owner".into(),
                    operator: "eq".into(),
                    value: serde_json::json!("u"),
                    column: None,
                })],
                cap: 3,
            },
            CapGuard::SumAtMost {
                field: "size".into(),
                filters: Vec::new(),
                add: 5,
                cap: 10,
            },
        ];
        let insert = InsertGuardedRequest {
            collection: "files".into(),
            data: HashMap::from([("size".to_string(), serde_json::json!(5))]),
            guards: guards.clone(),
        };
        let decoded: InsertGuardedRequest =
            codec::decode(&codec::encode(&insert).expect("encode")).expect("decode");
        assert_eq!(format!("{decoded:?}"), format!("{insert:?}"));

        let update = UpdateGuardedRequest {
            collection: "files".into(),
            filters: Vec::new(),
            data: HashMap::new(),
            guards,
        };
        let decoded: UpdateGuardedRequest =
            codec::decode(&codec::encode(&update).expect("encode")).expect("decode");
        assert_eq!(format!("{decoded:?}"), format!("{update:?}"));

        for response in [
            InsertGuardedResponse::Refused { guard: 1 },
            InsertGuardedResponse::Inserted {
                record: Record {
                    id: "1".into(),
                    data: HashMap::new(),
                },
            },
        ] {
            let decoded: InsertGuardedResponse =
                codec::decode(&codec::encode(&response).expect("encode")).expect("decode");
            assert_eq!(format!("{decoded:?}"), format!("{response:?}"));
        }
        for response in [
            UpdateGuardedResponse::Updated { rows_affected: 2 },
            UpdateGuardedResponse::Refused { guard: 0 },
            UpdateGuardedResponse::NoMatch,
        ] {
            let decoded: UpdateGuardedResponse =
                codec::decode(&codec::encode(&response).expect("encode")).expect("decode");
            assert_eq!(format!("{decoded:?}"), format!("{response:?}"));
        }
    }

    #[test]
    fn schema_lock_delete_where_count_request() {
        let req = DeleteWhereCountRequest {
            collection: String::new(),
            filters: vec![],
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82aa636f6c6c656374696f6ea0a766696c7465727390",
            "DeleteWhereCountRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_update_where_count_request() {
        let req = UpdateWhereCountRequest {
            collection: String::new(),
            filters: vec![],
            data: std::collections::HashMap::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "83aa636f6c6c656374696f6ea0a766696c7465727390a46461746180",
            "UpdateWhereCountRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_take_where_request() {
        let req = TakeWhereRequest {
            collection: String::new(),
            filters: vec![],
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82aa636f6c6c656374696f6ea0a766696c7465727390",
            "TakeWhereRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_delete_where_count_response() {
        let r = DeleteWhereCountResponse { count: 0 };
        let encoded = codec::encode(&r).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a5636f756e7400",
            "DeleteWhereCountResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_update_where_count_response() {
        let r = UpdateWhereCountResponse { count: 0 };
        let encoded = codec::encode(&r).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a5636f756e7400",
            "UpdateWhereCountResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_take_where_response() {
        let r = TakeWhereResponse { records: vec![] };
        let encoded = codec::encode(&r).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a77265636f72647390",
            "TakeWhereResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_increment_field_where_request() {
        let req = IncrementFieldWhereRequest {
            collection: String::new(),
            col: String::new(),
            delta: 0,
            filters: vec![],
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "84aa636f6c6c656374696f6ea0a3636f6ca0a564656c746100a766696c7465727390",
            "IncrementFieldWhereRequest schema changed — review consumer impact before updating this literal"
        );
    }
}

#[cfg(test)]
mod filter_node_tests {
    use super::*;

    // A legacy payload — a flat JSON array of leaf objects — must decode as
    // a Vec of Leaf nodes with no shape change.
    #[test]
    fn legacy_flat_array_decodes_as_leaves() {
        let json = r#"[{"field":"status","operator":"eq","value":"active"}]"#;
        let nodes: Vec<FilterNode> = serde_json::from_str(json).unwrap();
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            FilterNode::Leaf(f) => {
                assert_eq!(f.field, "status");
                assert_eq!(f.operator, "eq");
            }
            other => panic!("expected leaf, got {other:?}"),
        }
    }

    #[test]
    fn any_group_decodes() {
        let json = r#"{"any":[{"field":"a","value":1},{"field":"b","value":2}]}"#;
        let node: FilterNode = serde_json::from_str(json).unwrap();
        match node {
            FilterNode::Any { any } => assert_eq!(any.len(), 2),
            other => panic!("expected any-group, got {other:?}"),
        }
    }

    #[test]
    fn all_group_decodes() {
        let json = r#"{"all":[{"field":"a","value":1}]}"#;
        let node: FilterNode = serde_json::from_str(json).unwrap();
        assert!(matches!(node, FilterNode::All { .. }));
    }

    #[test]
    fn nested_group_decodes() {
        let json = r#"{"all":[{"field":"a","value":1},{"any":[{"field":"b","value":2}]}]}"#;
        let node: FilterNode = serde_json::from_str(json).unwrap();
        let FilterNode::All { all } = node else {
            panic!("expected all")
        };
        assert_eq!(all.len(), 2);
        assert!(matches!(all[1], FilterNode::Any { .. }));
    }

    #[test]
    fn leaf_and_group_shapes_are_disjoint() {
        // A leaf never has `all`/`any`; a group never has `field`. Untagged
        // matching picks Leaf first, so an object with `field` is a Leaf.
        let leaf: FilterNode = serde_json::from_str(r#"{"field":"x","value":1}"#).unwrap();
        assert!(matches!(leaf, FilterNode::Leaf(_)));
    }

    #[test]
    fn round_trips_through_json() {
        let node = FilterNode::All {
            all: vec![
                FilterNode::Leaf(FilterDef {
                    field: "a".into(),
                    operator: "eq".into(),
                    value: serde_json::json!(1),
                    column: None,
                }),
                FilterNode::Any {
                    any: vec![FilterNode::Leaf(FilterDef {
                        field: "b".into(),
                        operator: "gt".into(),
                        value: serde_json::json!(2),
                        column: None,
                    })],
                },
            ],
        };
        let s = serde_json::to_string(&node).unwrap();
        let back: FilterNode = serde_json::from_str(&s).unwrap();
        assert_eq!(format!("{node:?}"), format!("{back:?}"));
    }
}
