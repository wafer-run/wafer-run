use std::collections::HashMap;

#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
// `Filter`, `FilterOp`, `ListOptions`, `SortField` are defined in wafer_block::db;
// import them non-pub for use in conversion helpers and method signatures.
use wafer_block::db::{Filter, FilterOp, FilterTree, ListOptions, SortField};
// `Record` and `RecordList` are byte-identical to the wire types; collapse
// the duplicate by re-exporting from the wire crate.
pub use wafer_block::wire::database::{Record, RecordList};
use wafer_block::{
    common::{ErrorCode, ServiceOp},
    wire::database::{
        AddColumnRequest, AggregateRequest, ColumnDef, CountRequest, CountResponse, CreateRequest,
        DeleteRequest, DeleteWhereCountRequest, DeleteWhereCountResponse, DeleteWhereRequest,
        DropTableRequest, EnsureTableRequest, ExecRawRequest, ExecRawResponse,
        FilterDef as WireFilterDef, FilterNode, GetRequest, IncrementFieldWhereRequest,
        ListRequest, OnConflict, QueryRawRequest, SchemaOpResponse,
        SortFieldDef as WireSortFieldDef, SumRequest, SumResponse, TableDef, TableExistsRequest,
        TableExistsResponse, TakeWhereRequest, TakeWhereResponse, UpdateRequest,
        UpdateWhereCountRequest, UpdateWhereCountResponse, UpdateWhereRequest, UpsertRequest,
        UpsertResponse,
    },
    wrap::{DDL_RESOURCE, RAW_SQL_RESOURCE},
    WaferError,
};

use super::{call_service, decode, dual_api, svc, svc_fn};
// Re-export schema types for declarative table management.
pub use crate::interfaces::database::service::{
    col_blob, col_bool, col_datetime, col_float, col_int, col_int64, col_json, col_string,
    col_text, default_empty, default_false, default_int, default_now, default_null, default_string,
    default_true, default_zero, pk, pk_int, schema_soft_delete, timestamps, Column, DataType,
    DefaultVal, DefaultValue, Index, Reference, Table,
};

const BLOCK: &str = "wafer-run/database";

// --- Helpers ---

fn filter_op_str(op: &FilterOp) -> &'static str {
    match op {
        FilterOp::Equal => "eq",
        FilterOp::NotEqual => "neq",
        FilterOp::GreaterThan => "gt",
        FilterOp::GreaterEqual => "gte",
        FilterOp::LessThan => "lt",
        FilterOp::LessEqual => "lte",
        FilterOp::Like => "like",
        FilterOp::In => "in",
        FilterOp::IsNull => "is_null",
        FilterOp::IsNotNull => "is_not_null",
    }
}

/// Encode flat client-side [`Filter`]s as all-leaf wire [`FilterNode`]s. The
/// client never builds AND/OR groups; the tree shape exists so the wire is
/// uniform and group-capable callers (a later task) reuse the same field.
fn to_wire_filters(filters: &[Filter]) -> Vec<FilterNode> {
    filters
        .iter()
        .map(|f| {
            FilterNode::Leaf(WireFilterDef {
                field: f.field.clone(),
                operator: filter_op_str(&f.operator).to_string(),
                value: f.value.clone(),
            })
        })
        .collect()
}

/// Encode a builder-input [`FilterTree`] node as its wire [`FilterNode`]
/// shape — the inverse of the handler's `convert_filter_tree`
/// (`interfaces::database::handler`). A client that assembles an AND/OR
/// group via `ListOptions::filter_tree` (rather than the flat
/// `ListOptions::filters` fast path) needs this to put the group on the wire
/// at all.
pub(crate) fn filter_tree_to_wire_node(tree: &FilterTree) -> FilterNode {
    match tree {
        FilterTree::Leaf(f) => FilterNode::Leaf(WireFilterDef {
            field: f.field.clone(),
            operator: filter_op_str(&f.operator).to_string(),
            value: f.value.clone(),
        }),
        FilterTree::All(children) => FilterNode::All {
            all: children.iter().map(filter_tree_to_wire_node).collect(),
        },
        FilterTree::Any(children) => FilterNode::Any {
            any: children.iter().map(filter_tree_to_wire_node).collect(),
        },
    }
}

/// Build the wire `filters` field for `ListRequest` from a [`ListOptions`]:
/// the flat `opts.filters` (encoded as all-leaf nodes, as every other op
/// already does via [`to_wire_filters`]) concatenated with the converted
/// `opts.filter_tree` groups, when present. Top-level order doesn't matter —
/// the handler AND-combines every top-level `FilterNode` into one predicate
/// tree, so this is equivalent to "both sources, if set, must all hold".
fn list_wire_filters(opts: &ListOptions) -> Vec<FilterNode> {
    let mut nodes = to_wire_filters(&opts.filters);
    if let Some(tree) = &opts.filter_tree {
        nodes.extend(tree.iter().map(filter_tree_to_wire_node));
    }
    nodes
}

fn to_wire_sort(sort: &[SortField]) -> Vec<WireSortFieldDef> {
    sort.iter()
        .map(|s| WireSortFieldDef {
            field: s.field.clone(),
            desc: s.desc,
        })
        .collect()
}

// ===========================================================================
// Public API — generated as async (native) or sync (wasm-component)
// ===========================================================================

dual_api! {
    // --- Core CRUD ---

    /// Fetch a single record from `collection` by primary-key `id`.
    pub fn get(ctx, collection: &str, id: &str) -> Result<Record, WaferError> {
        let req = GetRequest { collection: collection.to_string(), id: id.to_string() };
        let data = svc!(ctx, BLOCK, ServiceOp::DATABASE_GET, &req, Some(collection), false, Some("db"))?;
        decode(&data)
    }

    /// List records in `collection` matching `opts` (filters, sort, limit, offset).
    pub fn list(ctx, collection: &str, opts: &ListOptions) -> Result<RecordList, WaferError> {
        let req = ListRequest {
            collection: collection.to_string(),
            filters: list_wire_filters(opts),
            sort: to_wire_sort(&opts.sort),
            limit: opts.limit,
            offset: opts.offset,
            skip_count: opts.skip_count,
            columns: opts.columns.clone(),
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_LIST,
            &req,
            Some(collection),
            false,
            Some("db")
        )?;
        decode(&data)
    }

    /// Insert a record into `collection` from `data` and return the stored row.
    pub fn create(ctx, collection: &str, data: HashMap<String, serde_json::Value>) -> Result<Record, WaferError> {
        let req = CreateRequest { collection: collection.to_string(), data };
        let resp = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_CREATE,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        decode(&resp)
    }

    /// Update the record `id` in `collection` with the fields in `data` and return the result.
    pub fn update(ctx, collection: &str, id: &str, data: HashMap<String, serde_json::Value>) -> Result<Record, WaferError> {
        let req = UpdateRequest { collection: collection.to_string(), id: id.to_string(), data };
        let resp = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_UPDATE,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        decode(&resp)
    }

    /// Delete the record `id` from `collection`.
    pub fn delete(ctx, collection: &str, id: &str) -> Result<(), WaferError> {
        let req = DeleteRequest { collection: collection.to_string(), id: id.to_string() };
        svc!(ctx, BLOCK, ServiceOp::DATABASE_DELETE, &req, Some(collection), true, Some("db"))?;
        Ok(())
    }

    /// Count records in `collection` matching `filters`.
    pub fn count(ctx, collection: &str, filters: &[Filter]) -> Result<i64, WaferError> {
        let req = CountRequest {
            collection: collection.to_string(),
            filters: to_wire_filters(filters),
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_COUNT,
            &req,
            Some(collection),
            false,
            Some("db")
        )?;
        let resp: CountResponse = decode(&data)?;
        Ok(resp.count)
    }

    /// Sum the numeric `field` across records in `collection` matching `filters`.
    pub fn sum(ctx, collection: &str, field: &str, filters: &[Filter]) -> Result<f64, WaferError> {
        let req = SumRequest {
            collection: collection.to_string(),
            field: field.to_string(),
            filters: to_wire_filters(filters),
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_SUM,
            &req,
            Some(collection),
            false,
            Some("db")
        )?;
        let resp: SumResponse = decode(&data)?;
        Ok(resp.sum)
    }

    /// **Admin escape hatch.** Runs raw SQL with row results; the SQL is
    /// opaque to the runtime so authorization is by the blanket
    /// `"__raw_sql__"` WRAP resource (admin-required). Block code MUST use
    /// the typed ops ([`list`], [`aggregate`], [`count`], …) instead — only
    /// the SQL explorer legitimately needs this.
    pub fn query_raw(ctx, query: &str, args: &[serde_json::Value]) -> Result<Vec<Record>, WaferError> {
        let req = QueryRawRequest { query: query.to_string(), args: args.to_vec() };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_QUERY_RAW,
            &req,
            Some(RAW_SQL_RESOURCE),
            false,
            Some("db")
        )?;
        decode(&data)
    }

    /// **Admin escape hatch.** Runs raw SQL with `rows_affected` return; the
    /// SQL is opaque to the runtime so authorization is by the blanket
    /// `"__raw_sql__"` WRAP resource (admin-required). Block code MUST use
    /// the typed ops ([`update_by_filters`], [`delete_by_filters`],
    /// [`upsert`], …) instead — only the SQL explorer and migration runners
    /// legitimately need this.
    pub fn exec_raw(ctx, query: &str, args: &[serde_json::Value]) -> Result<i64, WaferError> {
        let req = ExecRawRequest { query: query.to_string(), args: args.to_vec() };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_EXEC_RAW,
            &req,
            Some(RAW_SQL_RESOURCE),
            true,
            Some("db")
        )?;
        let resp: ExecRawResponse = decode(&data)?;
        Ok(resp.rows_affected)
    }

    /// Execute a DDL statement (`CREATE TABLE`, `CREATE INDEX`, `DROP TABLE`, etc).
    ///
    /// DDL routes through the `__ddl__` WRAP resource, which is permissive: any
    /// attributable caller can DDL (no admin required). This is intentional —
    /// each block is expected to DDL its own (`{org}__{block}__*`) tables on
    /// init via `migrations::apply` or equivalent, without needing the admin
    /// block in the loop.
    ///
    /// Convention (NOT enforced by parsing SQL here): blocks only DDL their own
    /// tables. Cross-block DDL through this entry point is a misuse caught by
    /// code review and the `scripts/audit-wrap-grants.sh` audit script.
    ///
    /// Use `exec_raw` for raw DML/DDL that legitimately requires the admin
    /// block (the SQL explorer, ad-hoc operator queries). `exec_raw` stays
    /// admin-gated under WRAP.
    pub fn ddl(ctx, statement: &str) -> Result<i64, WaferError> {
        let req = ExecRawRequest { query: statement.to_string(), args: vec![] };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_DDL,
            &req,
            Some(DDL_RESOURCE),
            true,
            Some("db")
        )?;
        let resp: ExecRawResponse = decode(&data)?;
        Ok(resp.rows_affected)
    }

    /// Create `table` (and its indexes) if absent. Authorized on the table
    /// name and on `__ddl__`; a block may only ensure its own
    /// `{org}__{block}__*` tables.
    pub fn ensure_table(ctx, table: &TableDef) -> Result<SchemaOpResponse, WaferError> {
        let req = EnsureTableRequest { table: table.clone() };
        let data = svc!(ctx, BLOCK, ServiceOp::DATABASE_ENSURE_TABLE, &req, Some(&table.name), true, Some("db"))?;
        decode(&data)
    }

    /// Add `column` to `table`. Authorized on the table name and `__ddl__`.
    pub fn add_column(ctx, table: &str, column: &ColumnDef) -> Result<SchemaOpResponse, WaferError> {
        let req = AddColumnRequest { table: table.to_string(), column: column.clone() };
        let data = svc!(ctx, BLOCK, ServiceOp::DATABASE_ADD_COLUMN, &req, Some(table), true, Some("db"))?;
        decode(&data)
    }

    /// Drop `table` if present. Authorized on the table name and `__ddl__`.
    pub fn drop_table(ctx, table: &str) -> Result<SchemaOpResponse, WaferError> {
        let req = DropTableRequest { table: table.to_string() };
        let data = svc!(ctx, BLOCK, ServiceOp::DATABASE_DROP_TABLE, &req, Some(table), true, Some("db"))?;
        decode(&data)
    }

    /// Whether `table` exists. A read authorized on the table name.
    pub fn table_exists(ctx, table: &str) -> Result<bool, WaferError> {
        let req = TableExistsRequest { table: table.to_string() };
        let data = svc!(ctx, BLOCK, ServiceOp::DATABASE_TABLE_EXISTS, &req, Some(table), false, Some("db"))?;
        let resp: TableExistsResponse = decode(&data)?;
        Ok(resp.exists)
    }

    // --- Higher-level helpers ---

    /// Fetch the first record in `collection` where `field == value`.
    /// Returns `Err(NOT_FOUND)` if no row matches.
    pub fn get_by_field(ctx, collection: &str, field: &str, value: serde_json::Value) -> Result<Record, WaferError> {
        let result = svc_fn!(ctx, list(
            collection,
            &ListOptions {
                filters: vec![Filter {
                    field: field.to_string(),
                    operator: FilterOp::Equal,
                    value,
                }],
                limit: 1,
                ..Default::default()
            }
        ))?;
        result
            .records
            .into_iter()
            .next()
            .ok_or_else(|| WaferError::new(ErrorCode::NotFound, "record not found"))
    }

    /// Update the record in `collection` whose `field == value`, or insert
    /// `data` if none exists.
    ///
    /// This is the **non-atomic** get-or-create: it issues a `get_by_field`
    /// followed by a separate `update`/`create`, so two concurrent callers can
    /// race (both miss the read, both insert). Its upside is flexibility —
    /// `field` needs no `UNIQUE`/`PRIMARY KEY` constraint. When `field` *is* a
    /// real conflict target, prefer the atomic [`upsert`], which issues a
    /// single `INSERT … ON CONFLICT …` round-trip with no race.
    pub fn upsert_by_field(
        ctx,
        collection: &str,
        field: &str,
        value: serde_json::Value,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, WaferError> {
        match svc_fn!(ctx, get_by_field(collection, field, value)) {
            Ok(existing) => svc_fn!(ctx, update(collection, &existing.id, data)),
            Err(e) if e.code == ErrorCode::NotFound => svc_fn!(ctx, create(collection, data)),
            Err(e) => Err(e),
        }
    }

    /// Insert `data` into `collection`, resolving a conflict on
    /// `conflict_columns` via `on_conflict`, in a single atomic
    /// `INSERT … ON CONFLICT …` round-trip. Returns rows affected.
    /// WRAP-authorized (write) against `collection`.
    ///
    /// `conflict_columns` must name a real `UNIQUE`/`PRIMARY KEY` conflict
    /// target. For a get-or-create on an unconstrained field, use the
    /// non-atomic [`upsert_by_field`] instead. Build `data`/`on_conflict` from
    /// `wafer_block::wire::database::OnConflict` (`SetColumns` or
    /// `WindowedCounter`).
    pub fn upsert(
        ctx,
        collection: &str,
        data: Vec<(String, serde_json::Value)>,
        conflict_columns: Vec<String>,
        on_conflict: OnConflict,
    ) -> Result<i64, WaferError> {
        let req = UpsertRequest {
            collection: collection.to_string(),
            data,
            conflict_columns,
            on_conflict,
        };
        let bytes = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_UPSERT,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        let resp: UpsertResponse = decode(&bytes)?;
        Ok(resp.rows_affected)
    }

    /// Run a grouped aggregate query described by `req` and return one
    /// [`Record`] per group (each carrying the aggregate aliases and any
    /// grouped columns / date buckets). WRAP-authorized (read) against
    /// `req.collection`.
    ///
    /// The runtime renders the SQL server-side from the structured request, so
    /// — unlike [`query_raw`] — no raw SQL crosses the boundary and the
    /// statement always targets the authorized collection. Build the aggregate
    /// and group-by terms from
    /// `wafer_block::wire::database::{AggregateColumnDef, GroupByDef}`.
    pub fn aggregate(ctx, req: AggregateRequest) -> Result<Vec<Record>, WaferError> {
        let collection = req.collection.clone();
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_AGGREGATE,
            &req,
            Some(collection.as_str()),
            false,
            Some("db")
        )?;
        decode(&data)
    }

    /// List all records matching the given filters.
    ///
    /// Intended for small, bounded collections (roles, permissions, legal docs).
    /// Hard-capped at 10,000 records — use paginated `list()` for larger collections.
    ///
    /// Sets `skip_count: true` on the underlying `ListOptions` so the
    /// backend avoids the `SELECT COUNT(*)` round-trip.
    pub fn list_all(ctx, collection: &str, filters: Vec<Filter>) -> Result<Vec<Record>, WaferError> {
        let result = svc_fn!(ctx, list(
            collection,
            &ListOptions {
                filters,
                limit: 10_000,
                skip_count: true,
                ..Default::default()
            }
        ))?;
        Ok(result.records)
    }

    /// List records matching `filters` in the order specified by `sort`.
    ///
    /// Hard-capped at 10,000 records. Skips the backend `COUNT` query — use
    /// `paginated_list` if you need `total_count` for pagination UI.
    ///
    /// Use this when the caller needs `ORDER BY` semantics but does not need
    /// pagination — most "show the N most recent X" or "list all X by name"
    /// queries fit. For unsorted bulk reads, prefer `list_all`.
    pub fn list_sorted(
        ctx,
        collection: &str,
        filters: Vec<Filter>,
        sort: Vec<SortField>,
    ) -> Result<Vec<Record>, WaferError> {
        let result = svc_fn!(ctx, list(
            collection,
            &ListOptions {
                filters,
                sort,
                limit: 10_000,
                skip_count: true,
                ..Default::default()
            }
        ))?;
        Ok(result.records)
    }

    /// Page through `collection` returning page `page` of `page_size` rows plus `total_count`.
    /// `page` and `page_size` are clamped to a minimum of 1 / 20 respectively.
    pub fn paginated_list(
        ctx,
        collection: &str,
        page: i64,
        page_size: i64,
        filters: Vec<Filter>,
        sort: Vec<SortField>,
    ) -> Result<RecordList, WaferError> {
        let page = if page < 1 { 1 } else { page };
        let page_size = if page_size < 1 { 20 } else { page_size };
        svc_fn!(ctx, list(
            collection,
            &ListOptions {
                filters,
                sort,
                limit: page_size,
                offset: (page - 1).saturating_mul(page_size),
                skip_count: false,
                filter_tree: None,
                columns: None,
            }
        ))
    }

    /// Soft-delete a record by setting `deleted_at` to the current UTC RFC3339 timestamp.
    pub fn soft_delete(ctx, collection: &str, id: &str) -> Result<Record, WaferError> {
        let mut data = HashMap::new();
        data.insert(
            "deleted_at".to_string(),
            serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
        );
        svc_fn!(ctx, update(collection, id, data))
    }

    /// Delete every record in `collection` where `field == value`.
    pub fn delete_by_field(
        ctx,
        collection: &str,
        field: &str,
        value: serde_json::Value,
    ) -> Result<(), WaferError> {
        svc_fn!(ctx, delete_by_filters(
            collection,
            vec![Filter {
                field: field.to_string(),
                operator: FilterOp::Equal,
                value,
            }]
        ))
    }

    /// Count records in `collection` where `field == value`.
    pub fn count_by_field(
        ctx,
        collection: &str,
        field: &str,
        value: serde_json::Value,
    ) -> Result<i64, WaferError> {
        svc_fn!(ctx, count(
            collection,
            &[Filter {
                field: field.to_string(),
                operator: FilterOp::Equal,
                value,
            }]
        ))
    }

    /// Delete every record in `collection` matching `filters`.
    pub fn delete_by_filters(ctx, collection: &str, filters: Vec<Filter>) -> Result<(), WaferError> {
        let req = DeleteWhereRequest {
            collection: collection.to_string(),
            filters: to_wire_filters(&filters),
        };
        svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_DELETE_WHERE,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        Ok(())
    }

    /// Delete all records matching the filters and return the number of deleted rows.
    pub fn delete_by_filters_count(ctx, collection: &str, filters: Vec<Filter>) -> Result<i64, WaferError> {
        let req = DeleteWhereCountRequest {
            collection: collection.to_string(),
            filters: to_wire_filters(&filters),
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_DELETE_WHERE_COUNT,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        let resp: DeleteWhereCountResponse = decode(&data)?;
        Ok(resp.count)
    }

    /// Atomically select and delete all records matching the filters, returning the deleted rows.
    pub fn take_by_filters(ctx, collection: &str, filters: Vec<Filter>) -> Result<Vec<Record>, WaferError> {
        let req = TakeWhereRequest {
            collection: collection.to_string(),
            filters: to_wire_filters(&filters),
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_TAKE_WHERE,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        let resp: TakeWhereResponse = decode(&data)?;
        Ok(resp.records)
    }

    /// Apply `data` as an update to every record in `collection` matching `filters`.
    pub fn update_by_filters(
        ctx,
        collection: &str,
        filters: Vec<Filter>,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), WaferError> {
        let req = UpdateWhereRequest {
            collection: collection.to_string(),
            filters: to_wire_filters(&filters),
            data,
        };
        svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_UPDATE_WHERE,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        Ok(())
    }

    /// Update all records matching the filters and return the number of updated rows.
    pub fn update_by_filters_count(
        ctx,
        collection: &str,
        filters: Vec<Filter>,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<i64, WaferError> {
        let req = UpdateWhereCountRequest {
            collection: collection.to_string(),
            filters: to_wire_filters(&filters),
            data,
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_UPDATE_WHERE_COUNT,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        let resp: UpdateWhereCountResponse = decode(&data)?;
        Ok(resp.count)
    }

    /// Atomically increment `col` by `delta` on every row in `collection`
    /// matching `filters`. Returns the number of rows modified. Use a negative
    /// `delta` to decrement. The backend issues a single
    /// `UPDATE … SET col = col + delta WHERE …` round-trip — no
    /// read-modify-write race, unlike a `list` + `update` sequence.
    pub fn increment_field_where(
        ctx,
        collection: &str,
        col: &str,
        delta: i64,
        filters: &[Filter],
    ) -> Result<i64, WaferError> {
        let req = IncrementFieldWhereRequest {
            collection: collection.to_string(),
            col: col.to_string(),
            delta,
            filters: to_wire_filters(filters),
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_INCREMENT_FIELD_WHERE,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        let resp: ExecRawResponse = decode(&data)?;
        Ok(resp.rows_affected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf_filter(field: &str) -> Filter {
        Filter {
            field: field.to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(1),
        }
    }

    #[test]
    fn filter_tree_to_wire_node_leaf() {
        let tree = FilterTree::Leaf(leaf_filter("a"));
        match filter_tree_to_wire_node(&tree) {
            FilterNode::Leaf(f) => {
                assert_eq!(f.field, "a");
                assert_eq!(f.operator, "eq");
            }
            other => panic!("expected Leaf, got {other:?}"),
        }
    }

    #[test]
    fn filter_tree_to_wire_node_any_group() {
        let tree = FilterTree::Any(vec![
            FilterTree::Leaf(leaf_filter("a")),
            FilterTree::Leaf(leaf_filter("b")),
        ]);
        match filter_tree_to_wire_node(&tree) {
            FilterNode::Any { any } => {
                assert_eq!(any.len(), 2);
                assert!(matches!(&any[0], FilterNode::Leaf(f) if f.field == "a"));
                assert!(matches!(&any[1], FilterNode::Leaf(f) if f.field == "b"));
            }
            other => panic!("expected Any, got {other:?}"),
        }
    }

    #[test]
    fn filter_tree_to_wire_node_nested_all_any_round_trips() {
        // All([Leaf(a), Any([Leaf(b), Leaf(c)])]) — mirrors the shape
        // SP-B2's OR-group migration produces.
        let tree = FilterTree::All(vec![
            FilterTree::Leaf(leaf_filter("a")),
            FilterTree::Any(vec![
                FilterTree::Leaf(leaf_filter("b")),
                FilterTree::Leaf(leaf_filter("c")),
            ]),
        ]);
        match filter_tree_to_wire_node(&tree) {
            FilterNode::All { all } => {
                assert_eq!(all.len(), 2);
                assert!(matches!(&all[0], FilterNode::Leaf(f) if f.field == "a"));
                match &all[1] {
                    FilterNode::Any { any } => {
                        assert_eq!(any.len(), 2);
                        assert!(matches!(&any[0], FilterNode::Leaf(f) if f.field == "b"));
                        assert!(matches!(&any[1], FilterNode::Leaf(f) if f.field == "c"));
                    }
                    other => panic!("expected Any, got {other:?}"),
                }
            }
            other => panic!("expected All, got {other:?}"),
        }
    }

    #[test]
    fn list_wire_filters_flat_only_produces_all_leaf_nodes() {
        let opts = ListOptions {
            filters: vec![leaf_filter("a"), leaf_filter("b")],
            ..Default::default()
        };
        let nodes = list_wire_filters(&opts);
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().all(|n| matches!(n, FilterNode::Leaf(_))));
    }

    #[test]
    fn list_wire_filters_tree_only_forwards_the_group() {
        let opts = ListOptions {
            filter_tree: Some(vec![FilterTree::Any(vec![
                FilterTree::Leaf(leaf_filter("a")),
                FilterTree::Leaf(leaf_filter("b")),
            ])]),
            ..Default::default()
        };
        let nodes = list_wire_filters(&opts);
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            FilterNode::Any { any } => assert_eq!(any.len(), 2),
            other => panic!("expected Any, got {other:?}"),
        }
    }

    #[test]
    fn list_wire_filters_concatenates_flat_and_tree() {
        let opts = ListOptions {
            filters: vec![leaf_filter("a")],
            filter_tree: Some(vec![FilterTree::Any(vec![
                FilterTree::Leaf(leaf_filter("b")),
                FilterTree::Leaf(leaf_filter("c")),
            ])]),
            ..Default::default()
        };
        let nodes = list_wire_filters(&opts);
        assert_eq!(nodes.len(), 2);
        assert!(matches!(&nodes[0], FilterNode::Leaf(f) if f.field == "a"));
        match &nodes[1] {
            FilterNode::Any { any } => assert_eq!(any.len(), 2),
            other => panic!("expected Any, got {other:?}"),
        }
    }

    #[test]
    fn list_request_columns_still_forwarded() {
        // Task 4 added `columns` projection to `ListRequest`; guard against a
        // future edit to `list()`'s request-building silently dropping it.
        let opts = ListOptions {
            columns: Some(vec!["id".to_string(), "name".to_string()]),
            ..Default::default()
        };
        let req = ListRequest {
            collection: "widgets".to_string(),
            filters: list_wire_filters(&opts),
            sort: to_wire_sort(&opts.sort),
            limit: opts.limit,
            offset: opts.offset,
            skip_count: opts.skip_count,
            columns: opts.columns.clone(),
        };
        assert_eq!(
            req.columns,
            Some(vec!["id".to_string(), "name".to_string()])
        );
    }
}
