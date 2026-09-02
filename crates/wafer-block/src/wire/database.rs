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

/// A single WHERE-clause predicate: `field <operator> value`.
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
    /// Maximum number of rows to return (0 = backend default).
    #[serde(default)]
    pub limit: i64,
    /// Number of rows to skip for pagination.
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
/// if they do not exist. Authorized on `table.name` and on `__ddl__`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsureTableRequest {
    /// The table to ensure exists.
    pub table: TableDef,
}

/// Request for `database.add_column`. Authorized on `table` and `__ddl__`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddColumnRequest {
    /// Table (collection) name.
    pub table: String,
    /// Column to add.
    pub column: ColumnDef,
}

/// Request for `database.drop_table`. Authorized on `table` and `__ddl__`.
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
    /// `SUM(field) AS alias`.
    Sum {
        /// Numeric column to sum.
        field: String,
        /// Output alias for the sum.
        alias: String,
    },
    /// `AVG(field) AS alias`.
    Avg {
        /// Numeric column to average.
        field: String,
        /// Output alias for the average.
        alias: String,
    },
    /// `MAX(field) AS alias` — greatest value in each group.
    Max {
        /// Column to take the maximum of.
        field: String,
        /// Output alias for the maximum.
        alias: String,
    },
    /// `SUM(CASE WHEN <when> THEN 1 ELSE 0 END) AS alias` — a portable
    /// conditional count (no `FILTER` clause required). `when` is a predicate
    /// forest, AND-combined at the top level; the handler bounds and validates
    /// it, and the server builds the `CASE` predicate (the sea-query
    /// expression is `!Send`, so it can't be built caller-side). An empty
    /// `when` is rejected as `InvalidArgument`.
    CaseWhenSum {
        /// Predicate whose matching rows are counted.
        when: Vec<FilterNode>,
        /// Output alias for the conditional count.
        alias: String,
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
            })],
            sort: vec![SortFieldDef {
                field: "created_at".into(),
                desc: true,
            }],
            limit: 50,
            offset: 100,
            skip_count: false,
            columns: Some(vec!["id".into(), "active".into()]),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ListRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.collection, original.collection);
        assert_eq!(decoded.limit, 50);
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
            limit: 0,
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
                },
                AggregateColumnDef::Avg {
                    field: "duration_ms".into(),
                    alias: "avg_ms".into(),
                },
                AggregateColumnDef::CaseWhenSum {
                    when: vec![FilterNode::Leaf(FilterDef {
                        field: "status".into(),
                        operator: "gte".into(),
                        value: serde_json::json!(400),
                    })],
                    alias: "errors".into(),
                },
            ],
            filters: vec![FilterNode::Leaf(FilterDef {
                field: "active".into(),
                operator: "eq".into(),
                value: serde_json::json!(true),
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
            limit: 0,
            offset: 0,
            skip_count: false,
            columns: None,
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "87aa636f6c6c656374696f6ea0a766696c7465727390a4736f727490a56c696d697400a66f666673657400aa736b69705f636f756e74c2a7636f6c756d6e73c0",
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
        assert_eq!(decoded.limit, 0);
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
                }),
                FilterNode::Any {
                    any: vec![FilterNode::Leaf(FilterDef {
                        field: "b".into(),
                        operator: "gt".into(),
                        value: serde_json::json!(2),
                    })],
                },
            ],
        };
        let s = serde_json::to_string(&node).unwrap();
        let back: FilterNode = serde_json::from_str(&s).unwrap();
        assert_eq!(format!("{node:?}"), format!("{back:?}"));
    }
}
