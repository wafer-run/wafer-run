//! Database service client — calls `wafer/database` block via `call-block`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::wafer::block_world::runtime;
use crate::wafer::block_world::types::{Action, Message};

/// A record returned from the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub data: HashMap<String, serde_json::Value>,
}

/// A paginated list of records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordList {
    pub records: Vec<Record>,
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}

/// Options for listing records.
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    pub filters: Vec<Filter>,
    pub sort: Vec<SortField>,
    pub limit: i64,
    pub offset: i64,
}

/// A filter condition.
#[derive(Debug, Clone)]
pub struct Filter {
    pub field: String,
    pub operator: FilterOp,
    pub value: serde_json::Value,
}

/// Filter operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Eq, Neq, Gt, Gte, Lt, Lte, Like, In, IsNull, IsNotNull,
}

/// A sort directive.
#[derive(Debug, Clone)]
pub struct SortField {
    pub field: String,
    pub desc: bool,
}

/// Database error type.
#[derive(Debug, Clone)]
pub struct DatabaseError {
    pub kind: String,
    pub message: String,
}

impl std::fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for DatabaseError {}

// --- Internal request/response types for serialization ---

#[derive(Serialize)]
struct GetReq<'a> {
    collection: &'a str,
    id: &'a str,
}

#[derive(Serialize)]
struct ListReq<'a> {
    collection: &'a str,
    filters: Vec<FilterSer<'a>>,
    sort: Vec<SortSer<'a>>,
    limit: i64,
    offset: i64,
}

#[derive(Serialize)]
struct FilterSer<'a> {
    field: &'a str,
    operator: &'a str,
    value: &'a serde_json::Value,
}

#[derive(Serialize)]
struct SortSer<'a> {
    field: &'a str,
    desc: bool,
}

#[derive(Serialize)]
struct CreateReq<'a> {
    collection: &'a str,
    data: &'a HashMap<String, serde_json::Value>,
}

#[derive(Serialize)]
struct UpdateReq<'a> {
    collection: &'a str,
    id: &'a str,
    data: &'a HashMap<String, serde_json::Value>,
}

#[derive(Serialize)]
struct DeleteReq<'a> {
    collection: &'a str,
    id: &'a str,
}

#[derive(Serialize)]
struct CountReq<'a> {
    collection: &'a str,
    filters: Vec<FilterSer<'a>>,
}

#[derive(Serialize)]
struct QueryRawReq<'a> {
    query: &'a str,
    args: &'a [serde_json::Value],
}

#[derive(Serialize)]
struct ExecRawReq<'a> {
    query: &'a str,
    args: &'a [serde_json::Value],
}

#[derive(Deserialize)]
struct ExecRawResp {
    rows_affected: i64,
}

#[derive(Deserialize)]
struct CountResp {
    count: i64,
}

// --- Helpers ---

fn filter_op_str(op: &FilterOp) -> &'static str {
    match op {
        FilterOp::Eq => "eq",
        FilterOp::Neq => "neq",
        FilterOp::Gt => "gt",
        FilterOp::Gte => "gte",
        FilterOp::Lt => "lt",
        FilterOp::Lte => "lte",
        FilterOp::Like => "like",
        FilterOp::In => "in",
        FilterOp::IsNull => "is_null",
        FilterOp::IsNotNull => "is_not_null",
    }
}

fn make_msg(kind: &str, data: &impl Serialize) -> Message {
    Message {
        kind: kind.to_string(),
        data: serde_json::to_vec(data).unwrap_or_default(),
        meta: Vec::new(),
    }
}

fn call_db(msg: &Message) -> Result<Vec<u8>, DatabaseError> {
    let result = runtime::call_block("wafer/database", msg);
    match result.action {
        Action::Error => {
            let err_msg = result.error
                .as_ref()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "unknown database error".to_string());
            if err_msg.contains("not found") {
                Err(DatabaseError { kind: "not_found".into(), message: err_msg })
            } else {
                Err(DatabaseError { kind: "internal".into(), message: err_msg })
            }
        }
        _ => Ok(result.response.map(|r| r.data).unwrap_or_default()),
    }
}

fn call_db_parse<T: serde::de::DeserializeOwned>(msg: &Message) -> Result<T, DatabaseError> {
    let data = call_db(msg)?;
    serde_json::from_slice(&data).map_err(|e| DatabaseError {
        kind: "internal".into(),
        message: format!("failed to parse response: {e}"),
    })
}

// --- Public API ---

/// Retrieve a single record by ID from a collection.
pub fn get(collection: &str, id: &str) -> Result<Record, DatabaseError> {
    let msg = make_msg("database.get", &GetReq { collection, id });
    call_db_parse(&msg)
}

/// List records with optional filtering, sorting, and pagination.
pub fn list(collection: &str, opts: &ListOptions) -> Result<RecordList, DatabaseError> {
    let filters: Vec<FilterSer> = opts.filters.iter().map(|f| FilterSer {
        field: &f.field,
        operator: filter_op_str(&f.operator),
        value: &f.value,
    }).collect();
    let sort: Vec<SortSer> = opts.sort.iter().map(|s| SortSer {
        field: &s.field,
        desc: s.desc,
    }).collect();
    let msg = make_msg("database.list", &ListReq {
        collection,
        filters,
        sort,
        limit: opts.limit,
        offset: opts.offset,
    });
    call_db_parse(&msg)
}

/// Create a new record in a collection.
pub fn create(collection: &str, data: &HashMap<String, serde_json::Value>) -> Result<Record, DatabaseError> {
    let msg = make_msg("database.create", &CreateReq { collection, data });
    call_db_parse(&msg)
}

/// Update an existing record by ID.
pub fn update(collection: &str, id: &str, data: &HashMap<String, serde_json::Value>) -> Result<Record, DatabaseError> {
    let msg = make_msg("database.update", &UpdateReq { collection, id, data });
    call_db_parse(&msg)
}

/// Delete a record by ID.
pub fn delete(collection: &str, id: &str) -> Result<(), DatabaseError> {
    let msg = make_msg("database.delete", &DeleteReq { collection, id });
    call_db(&msg)?;
    Ok(())
}

/// Count records matching filters.
pub fn count(collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError> {
    let filter_sers: Vec<FilterSer> = filters.iter().map(|f| FilterSer {
        field: &f.field,
        operator: filter_op_str(&f.operator),
        value: &f.value,
    }).collect();
    let msg = make_msg("database.count", &CountReq {
        collection,
        filters: filter_sers,
    });
    let resp: CountResp = call_db_parse(&msg)?;
    Ok(resp.count)
}

/// Execute a raw SELECT query.
pub fn query_raw(query: &str, args: &[serde_json::Value]) -> Result<Vec<Record>, DatabaseError> {
    let msg = make_msg("database.query_raw", &QueryRawReq { query, args });
    call_db_parse(&msg)
}

/// Execute a raw non-SELECT statement.
pub fn exec_raw(query: &str, args: &[serde_json::Value]) -> Result<i64, DatabaseError> {
    let msg = make_msg("database.exec_raw", &ExecRawReq { query, args });
    let resp: ExecRawResp = call_db_parse(&msg)?;
    Ok(resp.rows_affected)
}
