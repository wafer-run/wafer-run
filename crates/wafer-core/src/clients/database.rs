use std::collections::HashMap;

use serde::Serialize;

use wafer_run::common::{ErrorCode, ServiceOp};
use wafer_run::context::Context;
use wafer_run::types::WaferError;

// Re-export the data types so callers can use `clients::database::Record` etc.
pub use crate::blocks::database::service::{
    Filter, FilterOp, ListOptions, Record, RecordList, SortField,
};

// Re-export schema types for declarative table management.
pub use crate::blocks::database::service::{
    Table, Column, DataType, Index, Reference, DefaultValue, DefaultVal,
    pk, pk_int, col_string, col_text, col_int, col_int64, col_float,
    col_bool, col_datetime, col_json, col_blob, timestamps, schema_soft_delete,
    default_now, default_null, default_zero, default_empty, default_false,
    default_true, default_int, default_string,
};

use super::{call_service, decode};

const BLOCK: &str = "@wafer/database";

// --- Wire-format request types ---

#[derive(Serialize)]
struct GetReq<'a> {
    collection: &'a str,
    id: &'a str,
}

#[derive(Serialize)]
struct ListReq<'a> {
    collection: &'a str,
    filters: Vec<FilterDef<'a>>,
    sort: Vec<SortDef<'a>>,
    limit: i64,
    offset: i64,
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
    filters: Vec<FilterDef<'a>>,
}

#[derive(Serialize)]
struct SumReq<'a> {
    collection: &'a str,
    field: &'a str,
    filters: Vec<FilterDef<'a>>,
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

#[derive(Serialize)]
struct FilterDef<'a> {
    field: &'a str,
    operator: &'a str,
    value: &'a serde_json::Value,
}

#[derive(Serialize)]
struct SortDef<'a> {
    field: &'a str,
    desc: bool,
}

// --- Wire-format response types ---

#[derive(serde::Deserialize)]
struct CountResp {
    count: i64,
}

#[derive(serde::Deserialize)]
struct SumResp {
    sum: f64,
}

#[derive(serde::Deserialize)]
struct ExecRawResp {
    rows_affected: i64,
}

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

fn to_filter_defs(filters: &[Filter]) -> Vec<FilterDef<'_>> {
    filters
        .iter()
        .map(|f| FilterDef {
            field: &f.field,
            operator: filter_op_str(&f.operator),
            value: &f.value,
        })
        .collect()
}

fn to_sort_defs(sort: &[SortField]) -> Vec<SortDef<'_>> {
    sort.iter()
        .map(|s| SortDef {
            field: &s.field,
            desc: s.desc,
        })
        .collect()
}

// --- Public API: core CRUD ---

pub async fn get(ctx: &dyn Context, collection: &str, id: &str) -> Result<Record, WaferError> {
    let data = call_service(ctx, BLOCK, ServiceOp::DATABASE_GET, &GetReq { collection, id }).await?;
    decode(&data)
}

pub async fn list(ctx: &dyn Context, collection: &str, opts: &ListOptions) -> Result<RecordList, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::DATABASE_LIST,
        &ListReq {
            collection,
            filters: to_filter_defs(&opts.filters),
            sort: to_sort_defs(&opts.sort),
            limit: opts.limit,
            offset: opts.offset,
        },
    ).await?;
    decode(&data)
}

pub async fn create(
    ctx: &dyn Context,
    collection: &str,
    data: HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    let resp = call_service(
        ctx,
        BLOCK,
        ServiceOp::DATABASE_CREATE,
        &CreateReq {
            collection,
            data: &data,
        },
    ).await?;
    decode(&resp)
}

pub async fn update(
    ctx: &dyn Context,
    collection: &str,
    id: &str,
    data: HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    let resp = call_service(
        ctx,
        BLOCK,
        ServiceOp::DATABASE_UPDATE,
        &UpdateReq {
            collection,
            id,
            data: &data,
        },
    ).await?;
    decode(&resp)
}

pub async fn delete(ctx: &dyn Context, collection: &str, id: &str) -> Result<(), WaferError> {
    call_service(
        ctx,
        BLOCK,
        ServiceOp::DATABASE_DELETE,
        &DeleteReq { collection, id },
    ).await?;
    Ok(())
}

pub async fn count(ctx: &dyn Context, collection: &str, filters: &[Filter]) -> Result<i64, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::DATABASE_COUNT,
        &CountReq {
            collection,
            filters: to_filter_defs(filters),
        },
    ).await?;
    let resp: CountResp = decode(&data)?;
    Ok(resp.count)
}

pub async fn sum(
    ctx: &dyn Context,
    collection: &str,
    field: &str,
    filters: &[Filter],
) -> Result<f64, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::DATABASE_SUM,
        &SumReq {
            collection,
            field,
            filters: to_filter_defs(filters),
        },
    ).await?;
    let resp: SumResp = decode(&data)?;
    Ok(resp.sum)
}

pub async fn query_raw(
    ctx: &dyn Context,
    query: &str,
    args: &[serde_json::Value],
) -> Result<Vec<Record>, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::DATABASE_QUERY_RAW,
        &QueryRawReq { query, args },
    ).await?;
    decode(&data)
}

pub async fn exec_raw(
    ctx: &dyn Context,
    query: &str,
    args: &[serde_json::Value],
) -> Result<i64, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::DATABASE_EXEC_RAW,
        &ExecRawReq { query, args },
    ).await?;
    let resp: ExecRawResp = decode(&data)?;
    Ok(resp.rows_affected)
}

// --- Public API: higher-level helpers ---

/// Retrieve a single record where `field` equals `value`.
pub async fn get_by_field(
    ctx: &dyn Context,
    collection: &str,
    field: &str,
    value: serde_json::Value,
) -> Result<Record, WaferError> {
    let result = list(
        ctx,
        collection,
        &ListOptions {
            filters: vec![Filter {
                field: field.to_string(),
                operator: FilterOp::Equal,
                value,
            }],
            limit: 1,
            ..Default::default()
        },
    ).await?;
    result
        .records
        .into_iter()
        .next()
        .ok_or_else(|| WaferError::new(ErrorCode::NOT_FOUND, "record not found"))
}

/// Create or update a record based on a field match.
pub async fn upsert(
    ctx: &dyn Context,
    collection: &str,
    field: &str,
    value: serde_json::Value,
    data: HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    match get_by_field(ctx, collection, field, value).await {
        Ok(existing) => update(ctx, collection, &existing.id, data).await,
        Err(e) if e.code == ErrorCode::NOT_FOUND => create(ctx, collection, data).await,
        Err(e) => Err(e),
    }
}

/// Retrieve all records from a collection with optional filters.
pub async fn list_all(
    ctx: &dyn Context,
    collection: &str,
    filters: Vec<Filter>,
) -> Result<Vec<Record>, WaferError> {
    let result = list(
        ctx,
        collection,
        &ListOptions {
            filters,
            limit: 100000,
            ..Default::default()
        },
    ).await?;
    Ok(result.records)
}

/// Retrieve a paginated list of records.
pub async fn paginated_list(
    ctx: &dyn Context,
    collection: &str,
    page: i64,
    page_size: i64,
    filters: Vec<Filter>,
    sort: Vec<SortField>,
) -> Result<RecordList, WaferError> {
    let page = if page < 1 { 1 } else { page };
    let page_size = if page_size < 1 { 20 } else { page_size };
    list(
        ctx,
        collection,
        &ListOptions {
            filters,
            sort,
            limit: page_size,
            offset: (page - 1).saturating_mul(page_size),
        },
    ).await
}

/// Set `deleted_at` on a record (soft delete).
pub async fn soft_delete(
    ctx: &dyn Context,
    collection: &str,
    id: &str,
) -> Result<Record, WaferError> {
    let mut data = HashMap::new();
    data.insert(
        "deleted_at".to_string(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    update(ctx, collection, id, data).await
}

/// Delete all records where `field` equals `value`.
pub async fn delete_by_field(
    ctx: &dyn Context,
    collection: &str,
    field: &str,
    value: serde_json::Value,
) -> Result<(), WaferError> {
    let records = list_all(
        ctx,
        collection,
        vec![Filter {
            field: field.to_string(),
            operator: FilterOp::Equal,
            value,
        }],
    ).await?;
    for r in records {
        delete(ctx, collection, &r.id).await?;
    }
    Ok(())
}

/// Count records where `field` equals `value`.
pub async fn count_by_field(
    ctx: &dyn Context,
    collection: &str,
    field: &str,
    value: serde_json::Value,
) -> Result<i64, WaferError> {
    count(
        ctx,
        collection,
        &[Filter {
            field: field.to_string(),
            operator: FilterOp::Equal,
            value,
        }],
    ).await
}

/// Delete all records matching filters.
pub async fn delete_by_filters(
    ctx: &dyn Context,
    collection: &str,
    filters: Vec<Filter>,
) -> Result<(), WaferError> {
    let records = list_all(ctx, collection, filters).await?;
    for r in records {
        delete(ctx, collection, &r.id).await?;
    }
    Ok(())
}

/// Update all records matching filters.
pub async fn update_by_filters(
    ctx: &dyn Context,
    collection: &str,
    filters: Vec<Filter>,
    data: HashMap<String, serde_json::Value>,
) -> Result<(), WaferError> {
    let records = list_all(ctx, collection, filters).await?;
    for r in records {
        update(ctx, collection, &r.id, data.clone()).await?;
    }
    Ok(())
}
