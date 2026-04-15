//! Shared message handler logic for database blocks.
//!
//! Any block implementing the `database@v1` interface can delegate to these
//! functions to avoid duplicating the message protocol handling.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use wafer_block::common::{ErrorCode, ServiceOp};
use wafer_block::streams::output::OutputStream;
use wafer_block::*;
use wafer_run::schema::Table;

use super::service::{DatabaseError, DatabaseService, Filter, FilterOp, ListOptions, SortField};

// --- Request types ---

#[derive(Deserialize)]
struct GetRequest {
    collection: String,
    id: String,
}

#[derive(Deserialize)]
struct ListRequest {
    collection: String,
    #[serde(default)]
    filters: Vec<FilterDef>,
    #[serde(default)]
    sort: Vec<SortFieldDef>,
    #[serde(default)]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

#[derive(Deserialize)]
struct CreateRequest {
    collection: String,
    data: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct UpdateRequest {
    collection: String,
    id: String,
    data: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct DeleteRequest {
    collection: String,
    id: String,
}

#[derive(Deserialize)]
struct CountRequest {
    collection: String,
    #[serde(default)]
    filters: Vec<FilterDef>,
}

#[derive(Deserialize)]
struct SumRequest {
    collection: String,
    field: String,
    #[serde(default)]
    filters: Vec<FilterDef>,
}

#[derive(Deserialize)]
struct QueryRawRequest {
    query: String,
    #[serde(default)]
    args: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct ExecRawRequest {
    query: String,
    #[serde(default)]
    args: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct FilterDef {
    field: String,
    #[serde(default = "default_operator")]
    operator: String,
    #[serde(default)]
    value: serde_json::Value,
}

fn default_operator() -> String {
    "eq".to_string()
}

#[derive(Deserialize)]
struct SortFieldDef {
    field: String,
    #[serde(default)]
    desc: bool,
}

#[derive(Deserialize)]
struct DeleteWhereRequest {
    collection: String,
    #[serde(default)]
    filters: Vec<FilterDef>,
}

#[derive(Deserialize)]
struct UpdateWhereRequest {
    collection: String,
    #[serde(default)]
    filters: Vec<FilterDef>,
    data: HashMap<String, serde_json::Value>,
}

// --- Response types ---

#[derive(Serialize)]
struct CountResponse {
    count: i64,
}

#[derive(Serialize)]
struct ExecRawResponse {
    rows_affected: i64,
}

#[derive(Serialize)]
struct SumResponse {
    sum: f64,
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

fn convert_filters(defs: Vec<FilterDef>) -> Vec<Filter> {
    defs.into_iter()
        .map(|f| Filter {
            field: f.field,
            operator: parse_filter_op(&f.operator),
            value: f.value,
        })
        .collect()
}

fn convert_sort(defs: Vec<SortFieldDef>) -> Vec<SortField> {
    defs.into_iter()
        .map(|s| SortField {
            field: s.field,
            desc: s.desc,
        })
        .collect()
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

/// Serialize a value to JSON bytes and return as an OutputStream::respond,
/// or return an error stream if serialization fails.
fn to_output<T: serde::Serialize>(val: T) -> OutputStream {
    match serde_json::to_vec(&val) {
        Ok(bytes) => OutputStream::respond(bytes),
        Err(e) => OutputStream::error(WaferError::new(
            ErrorCode::INTERNAL,
            format!("serialize response: {e}"),
        )),
    }
}

macro_rules! decode_or_err {
    ($body:expr, $ty:ty, $op_name:expr) => {
        match serde_json::from_slice::<$ty>($body) {
            Ok(r) => r,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::INVALID_ARGUMENT,
                    format!("invalid {} request: {e}", $op_name),
                ))
            }
        }
    };
}

/// Handle a database message using the given service.
pub async fn handle_message(
    service: &dyn DatabaseService,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::DATABASE_GET => {
            let req = decode_or_err!(body, GetRequest, "database.get");
            match service.get(&req.collection, &req.id).await {
                Ok(record) => to_output(&record),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_LIST => {
            let req = decode_or_err!(body, ListRequest, "database.list");
            let opts = ListOptions {
                filters: convert_filters(req.filters),
                sort: convert_sort(req.sort),
                limit: req.limit,
                offset: req.offset,
            };
            match service.list(&req.collection, &opts).await {
                Ok(list) => to_output(&list),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_CREATE => {
            let req = decode_or_err!(body, CreateRequest, "database.create");
            match service.create(&req.collection, req.data).await {
                Ok(record) => to_output(&record),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_UPDATE => {
            let req = decode_or_err!(body, UpdateRequest, "database.update");
            match service.update(&req.collection, &req.id, req.data).await {
                Ok(record) => to_output(&record),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE => {
            let req = decode_or_err!(body, DeleteRequest, "database.delete");
            match service.delete(&req.collection, &req.id).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_COUNT => {
            let req = decode_or_err!(body, CountRequest, "database.count");
            let filters = convert_filters(req.filters);
            match service.count(&req.collection, &filters).await {
                Ok(count) => to_output(&CountResponse { count }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_QUERY_RAW => {
            let req = decode_or_err!(body, QueryRawRequest, "database.query_raw");
            match service.query_raw(&req.query, &req.args).await {
                Ok(records) => to_output(&records),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_SUM => {
            let req = decode_or_err!(body, SumRequest, "database.sum");
            let filters = convert_filters(req.filters);
            match service.sum(&req.collection, &req.field, &filters).await {
                Ok(sum) => to_output(&SumResponse { sum }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_EXEC_RAW => {
            let req = decode_or_err!(body, ExecRawRequest, "database.exec_raw");
            match service.exec_raw(&req.query, &req.args).await {
                Ok(rows) => to_output(&ExecRawResponse {
                    rows_affected: rows,
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE_WHERE => {
            let req = decode_or_err!(body, DeleteWhereRequest, "database.delete_where");
            let filters = convert_filters(req.filters);
            match service.delete_where(&req.collection, &filters).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_UPDATE_WHERE => {
            let req = decode_or_err!(body, UpdateWhereRequest, "database.update_where");
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
                WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("schema migration failed: {}", e),
                )
            })?;
            tracing::info!(tables = tables.len(), "database schema migrations applied");
        }
    }
    Ok(())
}
