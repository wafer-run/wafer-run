//! Database service client using WIT-generated imports.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::wafer::block_world::database as wit;

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

fn convert_wit_error(e: wit::DatabaseError) -> DatabaseError {
    match e {
        wit::DatabaseError::NotFound => DatabaseError { kind: "not_found".into(), message: "record not found".into() },
        wit::DatabaseError::Internal => DatabaseError { kind: "internal".into(), message: "internal database error".into() },
    }
}

fn record_from_wit(r: wit::DbRecord) -> Record {
    let data: HashMap<String, serde_json::Value> = serde_json::from_str(&r.data).unwrap_or_default();
    Record { id: r.id, data }
}

fn convert_filter(f: &Filter) -> wit::Filter {
    wit::Filter {
        field: f.field.clone(),
        operator: match f.operator {
            FilterOp::Eq => wit::FilterOp::Eq,
            FilterOp::Neq => wit::FilterOp::Neq,
            FilterOp::Gt => wit::FilterOp::Gt,
            FilterOp::Gte => wit::FilterOp::Gte,
            FilterOp::Lt => wit::FilterOp::Lt,
            FilterOp::Lte => wit::FilterOp::Lte,
            FilterOp::Like => wit::FilterOp::Like,
            FilterOp::In => wit::FilterOp::In,
            FilterOp::IsNull => wit::FilterOp::IsNull,
            FilterOp::IsNotNull => wit::FilterOp::IsNotNull,
        },
        value: serde_json::to_string(&f.value).unwrap_or_default(),
    }
}

fn convert_list_options(opts: &ListOptions) -> wit::ListOptions {
    wit::ListOptions {
        filters: opts.filters.iter().map(convert_filter).collect(),
        sort: opts.sort.iter().map(|s| wit::SortField { field: s.field.clone(), desc: s.desc }).collect(),
        limit: opts.limit,
        offset: opts.offset,
    }
}

/// Retrieve a single record by ID from a collection.
pub fn get(collection: &str, id: &str) -> Result<Record, DatabaseError> {
    wit::get(collection, id)
        .map(record_from_wit)
        .map_err(convert_wit_error)
}

/// List records with optional filtering, sorting, and pagination.
pub fn list(collection: &str, opts: &ListOptions) -> Result<RecordList, DatabaseError> {
    let wit_opts = convert_list_options(opts);
    wit::list(collection, &wit_opts)
        .map(|rl| RecordList {
            records: rl.records.into_iter().map(record_from_wit).collect(),
            total_count: rl.total_count,
            page: rl.page,
            page_size: rl.page_size,
        })
        .map_err(convert_wit_error)
}

/// Create a new record in a collection.
pub fn create(collection: &str, data: &HashMap<String, serde_json::Value>) -> Result<Record, DatabaseError> {
    let json = serde_json::to_string(data).unwrap_or_default();
    wit::create(collection, &json)
        .map(record_from_wit)
        .map_err(convert_wit_error)
}

/// Update an existing record by ID.
pub fn update(collection: &str, id: &str, data: &HashMap<String, serde_json::Value>) -> Result<Record, DatabaseError> {
    let json = serde_json::to_string(data).unwrap_or_default();
    wit::update(collection, id, &json)
        .map(record_from_wit)
        .map_err(convert_wit_error)
}

/// Delete a record by ID.
pub fn delete(collection: &str, id: &str) -> Result<(), DatabaseError> {
    wit::delete(collection, id).map_err(convert_wit_error)
}

/// Count records matching filters.
pub fn count(collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError> {
    let wit_filters: Vec<wit::Filter> = filters.iter().map(convert_filter).collect();
    wit::count(collection, &wit_filters).map_err(convert_wit_error)
}

/// Execute a raw SELECT query.
pub fn query_raw(query: &str, args: &[serde_json::Value]) -> Result<Vec<Record>, DatabaseError> {
    let args_json = serde_json::to_string(args).unwrap_or_default();
    wit::query_raw(query, &args_json)
        .map(|records| records.into_iter().map(record_from_wit).collect())
        .map_err(convert_wit_error)
}

/// Execute a raw non-SELECT statement.
pub fn exec_raw(query: &str, args: &[serde_json::Value]) -> Result<i64, DatabaseError> {
    let args_json = serde_json::to_string(args).unwrap_or_default();
    wit::exec_raw(query, &args_json).map_err(convert_wit_error)
}
