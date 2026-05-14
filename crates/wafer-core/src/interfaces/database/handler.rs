//! Shared message handler logic for database blocks.
//!
//! Any block implementing the `database@v1` interface can delegate to these
//! functions to avoid duplicating the message protocol handling.

use wafer_block::{
    common::{ErrorCode, ServiceOp},
    meta::META_WRAP_RESOURCE,
    streams::output::OutputStream,
    wire::database as wire,
    *,
};
use wafer_run::schema::Table;

use super::service::{
    self, DatabaseError, DatabaseService, Filter, FilterOp, ListOptions, SortField,
};
use crate::interfaces::handler_util::{decode_or_err, to_output};

/// SEC-003: enforce that the caller-supplied `wrap.resource` meta matches the
/// collection in the decoded payload. Empty meta = legacy path (runtime
/// already skipped WRAP); accept. The `__raw_sql__` / `__ddl__` pseudo-
/// resources are not checked here — they have their own admin/owner rules in
/// `wrap::check_access` that already gate query_raw/exec_raw/ddl.
fn check_collection(msg: &Message, expected: &str) -> Result<(), WaferError> {
    let supplied = msg.get_meta(META_WRAP_RESOURCE);
    if supplied.is_empty() || supplied == expected {
        Ok(())
    } else {
        Err(WaferError::new(
            ErrorCode::PERMISSION_DENIED,
            format!(
                "WRAP: wrap.resource meta '{supplied}' does not match payload collection '{expected}'"
            ),
        ))
    }
}

// --- Helpers ---

fn parse_filter_op(op: &str) -> FilterOp {
    match op {
        "eq" | "=" | "equal" => FilterOp::Equal,
        "neq" | "!=" | "not_equal" => FilterOp::NotEqual,
        "gt" | ">" | "greater_than" => FilterOp::GreaterThan,
        "gte" | ">=" | "greater_equal" => FilterOp::GreaterEqual,
        "lt" | "<" | "less_than" => FilterOp::LessThan,
        "lte" | "<=" | "less_equal" => FilterOp::LessEqual,
        "like" => FilterOp::Like,
        "in" => FilterOp::In,
        "is_null" => FilterOp::IsNull,
        "is_not_null" => FilterOp::IsNotNull,
        _ => FilterOp::Equal,
    }
}

fn convert_filters(defs: Vec<wire::FilterDef>) -> Vec<Filter> {
    defs.into_iter()
        .map(|f| Filter {
            field: f.field,
            operator: parse_filter_op(&f.operator),
            value: f.value,
        })
        .collect()
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

fn db_error_to_wafer(e: DatabaseError) -> WaferError {
    match e {
        DatabaseError::NotFound => WaferError::new(ErrorCode::NOT_FOUND, "record not found"),
        DatabaseError::Internal(msg) => {
            tracing::error!(error = %msg, "database internal error");
            WaferError::new(ErrorCode::INTERNAL, "internal database error")
        }
        DatabaseError::Other(err) => {
            tracing::error!(error = %err, "database error");
            WaferError::new(ErrorCode::INTERNAL, "internal database error")
        }
    }
}

/// Handle a database message using the given service.
pub async fn handle_message(
    service: &dyn DatabaseService,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::DATABASE_GET => {
            let req = decode_or_err!(body, wire::GetRequest, "database.get");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            match service.get(&req.collection, &req.id).await {
                Ok(record) => to_output(service_record_to_wire(record)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_LIST => {
            let req = decode_or_err!(body, wire::ListRequest, "database.list");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let opts = ListOptions {
                filters: convert_filters(req.filters),
                sort: convert_sort(req.sort),
                limit: req.limit,
                offset: req.offset,
                skip_count: req.skip_count,
            };
            match service.list(&req.collection, &opts).await {
                Ok(list) => to_output(service_record_list_to_wire(list)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_CREATE => {
            let req = decode_or_err!(body, wire::CreateRequest, "database.create");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            match service.create(&req.collection, req.data).await {
                Ok(record) => to_output(service_record_to_wire(record)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_UPDATE => {
            let req = decode_or_err!(body, wire::UpdateRequest, "database.update");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            match service.update(&req.collection, &req.id, req.data).await {
                Ok(record) => to_output(service_record_to_wire(record)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE => {
            let req = decode_or_err!(body, wire::DeleteRequest, "database.delete");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            match service.delete(&req.collection, &req.id).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_COUNT => {
            let req = decode_or_err!(body, wire::CountRequest, "database.count");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = convert_filters(req.filters);
            match service.count(&req.collection, &filters).await {
                Ok(count) => to_output(&wire::CountResponse { count }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_QUERY_RAW => {
            let req = decode_or_err!(body, wire::QueryRawRequest, "database.query_raw");
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
            let req = decode_or_err!(body, wire::SumRequest, "database.sum");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = convert_filters(req.filters);
            match service.sum(&req.collection, &req.field, &filters).await {
                Ok(sum) => to_output(&wire::SumResponse { sum }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_EXEC_RAW => {
            let req = decode_or_err!(body, wire::ExecRawRequest, "database.exec_raw");
            match service.exec_raw(&req.query, &req.args).await {
                Ok(rows) => to_output(&wire::ExecRawResponse {
                    rows_affected: rows,
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE_WHERE => {
            let req = decode_or_err!(body, wire::DeleteWhereRequest, "database.delete_where");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = convert_filters(req.filters);
            match service.delete_where(&req.collection, &filters).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE_WHERE_COUNT => {
            let req = decode_or_err!(
                body,
                wire::DeleteWhereCountRequest,
                "database.delete_where_count"
            );
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = convert_filters(req.filters);
            match service.delete_where_count(&req.collection, &filters).await {
                Ok(count) => to_output(&wire::DeleteWhereCountResponse { count }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_TAKE_WHERE => {
            let req = decode_or_err!(body, wire::TakeWhereRequest, "database.take_where");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = convert_filters(req.filters);
            match service.take_where(&req.collection, &filters).await {
                Ok(records) => to_output(&wire::TakeWhereResponse {
                    records: records.into_iter().map(service_record_to_wire).collect(),
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_UPDATE_WHERE => {
            let req = decode_or_err!(body, wire::UpdateWhereRequest, "database.update_where");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = convert_filters(req.filters);
            match service
                .update_where(&req.collection, &filters, req.data)
                .await
            {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
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
                WaferError::new(ErrorCode::INTERNAL, format!("schema migration failed: {e}"))
            })?;
            tracing::info!(tables = tables.len(), "database schema migrations applied");
        }
    }
    Ok(())
}
