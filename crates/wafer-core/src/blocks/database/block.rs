use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use wafer_run::block::{Block, BlockInfo};
use wafer_run::common::{ErrorCode, ServiceOp};
use wafer_run::context::Context;
use super::service::{
    DatabaseError, DatabaseService, Filter, FilterOp, ListOptions, SortField, Table,
};
use wafer_run::types::*;
use wafer_run::helpers::{respond_json, respond_empty};

/// DatabaseBlock wraps a DatabaseService and exposes it as a Block.
pub struct DatabaseBlock {
    service: Arc<dyn DatabaseService>,
    tables: Vec<Table>,
}

impl DatabaseBlock {
    pub fn new(service: Arc<dyn DatabaseService>, tables: Vec<Table>) -> Self {
        Self { service, tables }
    }
}

// --- Request types for deserializing message data ---

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
        DatabaseError::Internal(msg) => WaferError::new(ErrorCode::INTERNAL, msg),
        DatabaseError::Other(err) => WaferError::new(ErrorCode::INTERNAL, err.to_string()),
    }
}

impl Block for DatabaseBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer/database".to_string(),
            version: "0.1.0".to_string(),
            interface: "wafer.infra.database".to_string(),
            summary: "Database CRUD operations via DatabaseService".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }

    fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
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
                match self.service.get(&req.collection, &req.id) {
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
                match self.service.list(&req.collection, &opts) {
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
                match self.service.create(&req.collection, req.data) {
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
                match self.service.update(&req.collection, &req.id, req.data) {
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
                match self.service.delete(&req.collection, &req.id) {
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
                match self.service.count(&req.collection, &filters) {
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
                match self.service.query_raw(&req.query, &req.args) {
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
                match self.service.sum(&req.collection, &req.field, &filters) {
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
                match self.service.exec_raw(&req.query, &req.args) {
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

    fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init && !self.tables.is_empty() {
            self.service
                .ensure_schema_tables(&self.tables)
                .map_err(|e| WaferError::new("schema_migration", e.to_string()))?;
            tracing::info!(
                tables = self.tables.len(),
                "database schema migrations applied"
            );
        }
        Ok(())
    }
}
