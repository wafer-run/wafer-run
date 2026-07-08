//! Shared message handler logic for database blocks.
//!
//! Any block implementing the `database@v1` interface can delegate to these
//! functions to avoid duplicating the message protocol handling.

use wafer_block::{
    common::{ErrorCode, ServiceOp},
    db::{Filter, FilterOp, FilterTree, ListOptions, SortField},
    streams::output::OutputStream,
    types::ResourceType,
    wire::database as wire,
    wrap::{DDL_RESOURCE, RAW_SQL_RESOURCE},
    *,
};
use wafer_schema::Table;

use super::service::{self, DatabaseError, DatabaseService};
use crate::interfaces::handler_util::{decode_and_authorize, to_output};

// --- Helpers ---

/// Maximum nesting depth of a `FilterNode` tree accepted from the wire.
pub(crate) const MAX_FILTER_DEPTH: usize = 16;
/// Maximum total node count of a `FilterNode` tree accepted from the wire.
pub(crate) const MAX_FILTER_NODES: usize = 256;

fn invalid(msg: impl Into<String>) -> WaferError {
    WaferError::new(ErrorCode::InvalidArgument, msg)
}

/// Convert a wire `FilterNode` forest into builder-input `FilterTree`,
/// rejecting trees that exceed the depth or node-count bounds, reject unknown
/// filter operators, and reject nested empty `all`/`any` groups (which would
/// otherwise collapse to a degenerate always-true/always-false condition).
/// Total and panic-free on any input.
///
/// A top-level empty forest (`[]`) is valid and means "no filter"; only
/// *nested* empty groups are rejected.
pub(crate) fn convert_filter_tree(
    nodes: Vec<wire::FilterNode>,
) -> Result<Vec<FilterTree>, WaferError> {
    let mut count = 0usize;
    nodes
        .into_iter()
        .map(|n| convert_node(n, 1, &mut count))
        .collect()
}

fn convert_node(
    node: wire::FilterNode,
    depth: usize,
    count: &mut usize,
) -> Result<FilterTree, WaferError> {
    if depth > MAX_FILTER_DEPTH {
        return Err(invalid("filter tree too deep"));
    }
    *count += 1;
    if *count > MAX_FILTER_NODES {
        return Err(invalid("filter tree has too many nodes"));
    }
    match node {
        wire::FilterNode::Leaf(f) => Ok(FilterTree::Leaf(Filter {
            field: f.field,
            operator: FilterOp::parse_wire(&f.operator).map_err(|e| invalid(e.to_string()))?,
            value: f.value,
        })),
        wire::FilterNode::All { all } => {
            if all.is_empty() {
                return Err(invalid("filter group must have at least one child"));
            }
            Ok(FilterTree::All(
                all.into_iter()
                    .map(|c| convert_node(c, depth + 1, count))
                    .collect::<Result<_, _>>()?,
            ))
        }
        wire::FilterNode::Any { any } => {
            if any.is_empty() {
                return Err(invalid("filter group must have at least one child"));
            }
            Ok(FilterTree::Any(
                any.into_iter()
                    .map(|c| convert_node(c, depth + 1, count))
                    .collect::<Result<_, _>>()?,
            ))
        }
    }
}

/// Flatten a tree to a leaf-only `Vec<Filter>`, rejecting any group node.
/// Used by ops whose builders take a flat `&[Filter]` and which no current
/// caller invokes with a group; a group here is a client/runtime mismatch, so
/// fail closed rather than silently drop it.
pub(crate) fn flatten_leaves(tree: &[FilterTree]) -> Result<Vec<Filter>, WaferError> {
    let mut out = Vec::with_capacity(tree.len());
    for node in tree {
        match node {
            FilterTree::Leaf(f) => out.push(f.clone()),
            FilterTree::All(_) | FilterTree::Any(_) => {
                return Err(invalid("operation does not support filter groups"));
            }
        }
    }
    Ok(out)
}

fn convert_sort(defs: Vec<wire::SortFieldDef>) -> Vec<SortField> {
    defs.into_iter()
        .map(|s| SortField {
            field: s.field,
            desc: s.desc,
        })
        .collect()
}

fn service_record_to_wire(r: service::Record) -> wire::Record {
    wire::Record {
        id: r.id,
        data: r.data,
    }
}

fn service_record_list_to_wire(l: service::RecordList) -> wire::RecordList {
    wire::RecordList {
        records: l.records.into_iter().map(service_record_to_wire).collect(),
        total_count: l.total_count,
        page: l.page,
        page_size: l.page_size,
    }
}

/// Substrings of structured backend errors that are safe to surface to
/// callers. These are operator-authored DDL outcomes (column names,
/// table names) — never user-supplied content — so they don't leak
/// secrets, and consumers like `solobase-core::migration_helper` need to
/// see them to decide whether a failure is benign (e.g. re-running an
/// `ALTER TABLE … ADD COLUMN` after the column already exists).
///
/// Every other internal error message stays scrubbed.
const PRESERVED_DB_ERROR_SUBSTRINGS: &[&str] = &[
    // SQLite / D1
    "duplicate column name",
    // PostgreSQL
    "already exists",
];

fn is_preserved_db_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    PRESERVED_DB_ERROR_SUBSTRINGS
        .iter()
        .any(|needle| lower.contains(needle))
}

fn db_error_to_wafer(e: DatabaseError) -> WaferError {
    match e {
        DatabaseError::NotFound => WaferError::new(ErrorCode::NotFound, "record not found"),
        DatabaseError::Internal(msg) => {
            if is_preserved_db_error(&msg) {
                tracing::warn!(error = %msg, "database structured error (preserved)");
                WaferError::new(ErrorCode::Internal, msg)
            } else {
                tracing::error!(error = %msg, "database internal error");
                WaferError::new(ErrorCode::Internal, "internal database error")
            }
        }
        DatabaseError::Other(err) => {
            let msg = err.to_string();
            if is_preserved_db_error(&msg) {
                tracing::warn!(error = %msg, "database structured error (preserved)");
                WaferError::new(ErrorCode::Internal, msg)
            } else {
                tracing::error!(error = %msg, "database error");
                WaferError::new(ErrorCode::Internal, "internal database error")
            }
        }
    }
}

/// Handle a database message using the given service.
///
/// `ctx` is the trusted host-side authorization surface: every op arm that
/// touches a WRAP-governed resource authorizes via
/// [`decode_and_authorize`], which bundles the codec decode with a call to
/// `ctx.check_resource_access` so an arm cannot obtain its typed request
/// without also being checked.
pub async fn handle_message(
    service: &dyn DatabaseService,
    ctx: &dyn Context,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::DATABASE_GET => {
            let req =
                match decode_and_authorize::<wire::GetRequest>(ctx, body, "database.get", |r| {
                    (r.collection.clone(), ResourceType::Db, false)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            match service.get(&req.collection, &req.id).await {
                Ok(record) => to_output(service_record_to_wire(record)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_LIST => {
            let req =
                match decode_and_authorize::<wire::ListRequest>(ctx, body, "database.list", |r| {
                    (r.collection.clone(), ResourceType::Db, false)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            if matches!(&req.columns, Some(c) if c.is_empty()) {
                return OutputStream::error(invalid("columns must be non-empty when specified"));
            }
            // All LIST filtering — flat or group — flows through
            // `filter_tree`; `DbExec::list` renders it via
            // `query::build_condition_tree` as the `extra_condition` AND-ed
            // onto the (now-always-empty) flat `filters` clause. `filters`
            // stays empty here rather than the flattened leaves: keeping both
            // populated would double-apply flat predicates (once via
            // `opts.filters`, once via the `filter_tree` leaves already
            // covering them).
            let opts = ListOptions {
                filters: Vec::new(),
                sort: convert_sort(req.sort),
                limit: req.limit,
                offset: req.offset,
                skip_count: req.skip_count,
                filter_tree: Some(tree),
                columns: req.columns,
            };
            match service.list(&req.collection, &opts).await {
                Ok(list) => to_output(service_record_list_to_wire(list)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_CREATE => {
            let req = match decode_and_authorize::<wire::CreateRequest>(
                ctx,
                body,
                "database.create",
                |r| (r.collection.clone(), ResourceType::Db, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.create(&req.collection, req.data).await {
                Ok(record) => to_output(service_record_to_wire(record)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_UPDATE => {
            let req = match decode_and_authorize::<wire::UpdateRequest>(
                ctx,
                body,
                "database.update",
                |r| (r.collection.clone(), ResourceType::Db, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.update(&req.collection, &req.id, req.data).await {
                Ok(record) => to_output(service_record_to_wire(record)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE => {
            let req = match decode_and_authorize::<wire::DeleteRequest>(
                ctx,
                body,
                "database.delete",
                |r| (r.collection.clone(), ResourceType::Db, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.delete(&req.collection, &req.id).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_COUNT => {
            let req =
                match decode_and_authorize::<wire::CountRequest>(ctx, body, "database.count", |r| {
                    (r.collection.clone(), ResourceType::Db, false)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.count(&req.collection, &filters).await {
                Ok(count) => to_output(&wire::CountResponse { count }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_QUERY_RAW => {
            let req = match decode_and_authorize::<wire::QueryRawRequest>(
                ctx,
                body,
                "database.query_raw",
                |_r| (RAW_SQL_RESOURCE.to_string(), ResourceType::Db, false),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.query_raw(&req.query, &req.args).await {
                Ok(records) => {
                    let wire_records: Vec<wire::Record> =
                        records.into_iter().map(service_record_to_wire).collect();
                    to_output(&wire_records)
                }
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_SUM => {
            let req =
                match decode_and_authorize::<wire::SumRequest>(ctx, body, "database.sum", |r| {
                    (r.collection.clone(), ResourceType::Db, false)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.sum(&req.collection, &req.field, &filters).await {
                Ok(sum) => to_output(&wire::SumResponse { sum }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_EXEC_RAW => {
            let req = match decode_and_authorize::<wire::ExecRawRequest>(
                ctx,
                body,
                "database.exec_raw",
                |_r| (RAW_SQL_RESOURCE.to_string(), ResourceType::Db, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.exec_raw(&req.query, &req.args).await {
                Ok(rows) => to_output(&wire::ExecRawResponse {
                    rows_affected: rows,
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DDL => {
            // Host-authoritative DDL sentinel (distinct op from
            // `DATABASE_EXEC_RAW` so a caller can't relabel a DDL statement
            // as a plain exec_raw, or vice versa, to dodge the `__ddl__`
            // resource check).
            let req = match decode_and_authorize::<wire::ExecRawRequest>(
                ctx,
                body,
                "database.ddl",
                |_r| (DDL_RESOURCE.to_string(), ResourceType::Db, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.exec_raw(&req.query, &req.args).await {
                Ok(rows) => to_output(&wire::ExecRawResponse {
                    rows_affected: rows,
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        // SP-B: execute/query still ship raw SQL; collection is a label
        // until structured queries land. WRAP authorizes against
        // `req.collection`, but the backend runs `req.sql` verbatim, so a
        // caller with a grant for collection A can still run arbitrary SQL
        // against collection B by mislabeling `collection`. Closing this is
        // SP-B's job (structured statements the runtime can actually
        // validate), not this task's.
        ServiceOp::DATABASE_EXECUTE => {
            let req = match decode_and_authorize::<wire::ExecuteRequest>(
                ctx,
                body,
                "database.execute",
                |r| (r.collection.clone(), ResourceType::Db, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.exec_raw(&req.sql, &req.args).await {
                Ok(rows_affected) => to_output(&wire::ExecuteResponse { rows_affected }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        // SP-B: see the identical note on DATABASE_EXECUTE above.
        ServiceOp::DATABASE_QUERY => {
            let req =
                match decode_and_authorize::<wire::QueryRequest>(ctx, body, "database.query", |r| {
                    (r.collection.clone(), ResourceType::Db, false)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            match service.query_raw(&req.sql, &req.args).await {
                Ok(records) => to_output(&wire::QueryResponse {
                    rows: records.into_iter().map(service_record_to_wire).collect(),
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE_WHERE => {
            let req = match decode_and_authorize::<wire::DeleteWhereRequest>(
                ctx,
                body,
                "database.delete_where",
                |r| (r.collection.clone(), ResourceType::Db, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.delete_where(&req.collection, &filters).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE_WHERE_COUNT => {
            let req = match decode_and_authorize::<wire::DeleteWhereCountRequest>(
                ctx,
                body,
                "database.delete_where_count",
                |r| (r.collection.clone(), ResourceType::Db, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.delete_where_count(&req.collection, &filters).await {
                Ok(count) => to_output(&wire::DeleteWhereCountResponse { count }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_TAKE_WHERE => {
            let req = match decode_and_authorize::<wire::TakeWhereRequest>(
                ctx,
                body,
                "database.take_where",
                |r| (r.collection.clone(), ResourceType::Db, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.take_where(&req.collection, &filters).await {
                Ok(records) => to_output(&wire::TakeWhereResponse {
                    records: records.into_iter().map(service_record_to_wire).collect(),
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_UPDATE_WHERE => {
            let req = match decode_and_authorize::<wire::UpdateWhereRequest>(
                ctx,
                body,
                "database.update_where",
                |r| (r.collection.clone(), ResourceType::Db, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service
                .update_where(&req.collection, &filters, req.data)
                .await
            {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_INCREMENT_FIELD_WHERE => {
            let req = match decode_and_authorize::<wire::IncrementFieldWhereRequest>(
                ctx,
                body,
                "database.increment_field_where",
                |r| (r.collection.clone(), ResourceType::Db, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service
                .increment_field_where(&req.collection, &req.col, req.delta, &filters)
                .await
            {
                Ok(rows) => to_output(&wire::ExecRawResponse {
                    rows_affected: rows,
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("unknown database operation: {other}"),
        )),
    }
}

/// Handle database lifecycle events (schema migration on Init).
pub async fn handle_lifecycle(
    service: &dyn DatabaseService,
    tables: &[Table],
    event: &LifecycleEvent,
) -> std::result::Result<(), WaferError> {
    if event.event_type == LifecycleType::Init {
        if tables.is_empty() {
            tracing::debug!("no schema tables configured — skipping migration");
        } else {
            service.ensure_schema_tables(tables).await.map_err(|e| {
                WaferError::new(ErrorCode::Internal, format!("schema migration failed: {e}"))
            })?;
            tracing::info!(tables = tables.len(), "database schema migrations applied");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_duplicate_column_error_messages() {
        // SQLite / D1 wording
        let w = db_error_to_wafer(DatabaseError::Internal(
            "duplicate column name: block".to_string(),
        ));
        assert_eq!(w.code, ErrorCode::Internal);
        assert!(
            w.message.contains("duplicate column name"),
            "expected preserved message, got: {}",
            w.message
        );

        // PostgreSQL wording
        let w = db_error_to_wafer(DatabaseError::Internal(
            r#"column "block" of relation "variables" already exists"#.to_string(),
        ));
        assert!(
            w.message.contains("already exists"),
            "expected preserved message, got: {}",
            w.message
        );
    }

    #[test]
    fn scrubs_generic_internal_errors() {
        // Random backend internal failures still get scrubbed so we don't
        // leak driver internals or connection strings.
        let w = db_error_to_wafer(DatabaseError::Internal(
            "connection refused: tcp://10.0.0.5:5432".to_string(),
        ));
        assert_eq!(w.message, "internal database error");
    }

    #[test]
    fn not_found_stays_descriptive() {
        let w = db_error_to_wafer(DatabaseError::NotFound);
        assert_eq!(w.code, ErrorCode::NotFound);
        assert_eq!(w.message, "record not found");
    }

    // `FilterOp::parse_wire` unit tests live next to the parser in
    // `wafer-block/src/db.rs`; the handler's use of it (including bad-operator
    // rejection) is covered by `filter_tree_conversion_tests` below.
}

#[cfg(test)]
mod filter_tree_conversion_tests {
    use wafer_block::wire::database::{FilterDef, FilterNode};

    use super::{convert_filter_tree, flatten_leaves, MAX_FILTER_DEPTH, MAX_FILTER_NODES};

    fn leaf(field: &str) -> FilterNode {
        FilterNode::Leaf(FilterDef {
            field: field.into(),
            operator: "eq".into(),
            value: serde_json::json!(1),
        })
    }

    #[test]
    fn flat_leaves_convert() {
        let tree = convert_filter_tree(vec![leaf("a"), leaf("b")]).unwrap();
        assert_eq!(tree.len(), 2);
        let flat = flatten_leaves(&tree).unwrap();
        assert_eq!(flat.len(), 2);
    }

    #[test]
    fn top_level_empty_is_ok() {
        // The top-level empty filter list means "no filter" and must stay
        // valid — only nested empty groups are rejected.
        let tree = convert_filter_tree(vec![]).unwrap();
        assert!(tree.is_empty());
        assert!(flatten_leaves(&tree).unwrap().is_empty());
    }

    #[test]
    fn group_is_rejected_by_flatten() {
        let tree = convert_filter_tree(vec![FilterNode::Any {
            any: vec![leaf("a")],
        }])
        .unwrap();
        let err = flatten_leaves(&tree).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }

    #[test]
    fn empty_group_is_rejected() {
        // A nested empty `all`/`any` group would otherwise convert to a
        // degenerate empty `Cond`; reject it so conversion stays fail-closed.
        let err = convert_filter_tree(vec![FilterNode::All { all: vec![] }]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
        let err = convert_filter_tree(vec![FilterNode::Any { any: vec![] }]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }

    #[test]
    fn depth_over_limit_is_rejected() {
        // Nest All groups MAX_FILTER_DEPTH+1 deep.
        let mut node = leaf("a");
        for _ in 0..(MAX_FILTER_DEPTH + 1) {
            node = FilterNode::All { all: vec![node] };
        }
        let err = convert_filter_tree(vec![node]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }

    #[test]
    fn node_count_over_limit_is_rejected() {
        let many: Vec<FilterNode> = (0..(MAX_FILTER_NODES + 1)).map(|_| leaf("a")).collect();
        let err = convert_filter_tree(vec![FilterNode::All { all: many }]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }

    #[test]
    fn bad_operator_is_rejected() {
        let bad = FilterNode::Leaf(FilterDef {
            field: "a".into(),
            operator: "no_such_op".into(),
            value: serde_json::json!(1),
        });
        let err = convert_filter_tree(vec![bad]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }
}
