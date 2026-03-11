//! Shared message handler logic for database blocks.
//!
//! Any block implementing the `database@v1` interface can delegate to these
//! functions to avoid duplicating the message protocol handling.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use wafer_run::common::{ErrorCode, ServiceOp};
use wafer_run::helpers::{respond_empty, respond_json};
use wafer_run::schema::Table;
use wafer_run::types::*;

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

/// Handle a database message using the given service.
pub async fn handle_message(service: &dyn DatabaseService, msg: &mut Message) -> Result_ {
    match msg.kind.as_str() {
        ServiceOp::DATABASE_GET => {
            let req: GetRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid database.get request: {e}"),
                    ))
                }
            };
            match service.get(&req.collection, &req.id).await {
                Ok(record) => respond_json(msg, &record),
                Err(e) => Result_::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_LIST => {
            let req: ListRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid database.list request: {e}"),
                    ))
                }
            };
            let opts = ListOptions {
                filters: convert_filters(req.filters),
                sort: convert_sort(req.sort),
                limit: req.limit,
                offset: req.offset,
            };
            match service.list(&req.collection, &opts).await {
                Ok(list) => respond_json(msg, &list),
                Err(e) => Result_::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_CREATE => {
            let req: CreateRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid database.create request: {e}"),
                    ))
                }
            };
            match service.create(&req.collection, req.data).await {
                Ok(record) => respond_json(msg, &record),
                Err(e) => Result_::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_UPDATE => {
            let req: UpdateRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid database.update request: {e}"),
                    ))
                }
            };
            match service.update(&req.collection, &req.id, req.data).await {
                Ok(record) => respond_json(msg, &record),
                Err(e) => Result_::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE => {
            let req: DeleteRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid database.delete request: {e}"),
                    ))
                }
            };
            match service.delete(&req.collection, &req.id).await {
                Ok(()) => respond_empty(msg),
                Err(e) => Result_::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_COUNT => {
            let req: CountRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid database.count request: {e}"),
                    ))
                }
            };
            let filters = convert_filters(req.filters);
            match service.count(&req.collection, &filters).await {
                Ok(count) => respond_json(msg, &CountResponse { count }),
                Err(e) => Result_::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_QUERY_RAW => {
            let req: QueryRawRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid database.query_raw request: {e}"),
                    ))
                }
            };
            match service.query_raw(&req.query, &req.args).await {
                Ok(records) => respond_json(msg, &records),
                Err(e) => Result_::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_SUM => {
            let req: SumRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid database.sum request: {e}"),
                    ))
                }
            };
            let filters = convert_filters(req.filters);
            match service.sum(&req.collection, &req.field, &filters).await {
                Ok(sum) => respond_json(msg, &SumResponse { sum }),
                Err(e) => Result_::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_EXEC_RAW => {
            let req: ExecRawRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid database.exec_raw request: {e}"),
                    ))
                }
            };
            match service.exec_raw(&req.query, &req.args).await {
                Ok(rows) => respond_json(msg, &ExecRawResponse { rows_affected: rows }),
                Err(e) => Result_::error(db_error_to_wafer(e)),
            }
        }
        other => Result_::error(WaferError::new(
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
            tracing::info!(
                tables = tables.len(),
                "database schema migrations applied"
            );
        }
    }
    Ok(())
}
