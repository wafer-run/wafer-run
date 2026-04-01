use std::collections::HashMap;

use serde::Serialize;

use wafer_block::common::{ErrorCode, ServiceOp};
#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
use wafer_block::WaferError;

// Re-export the data types so callers can use `clients::database::Record` etc.
pub use crate::interfaces::database::service::{
    Filter, FilterOp, ListOptions, Record, RecordList, SortField,
};

// Re-export schema types for declarative table management.
pub use crate::interfaces::database::service::{
    col_blob, col_bool, col_datetime, col_float, col_int, col_int64, col_json, col_string,
    col_text, default_empty, default_false, default_int, default_now, default_null, default_string,
    default_true, default_zero, pk, pk_int, schema_soft_delete, timestamps, Column, DataType,
    DefaultVal, DefaultValue, Index, Reference, Table,
};

use super::{call_service, decode};

const BLOCK: &str = "wafer-run/database";

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

// ===========================================================================
// Public API: core CRUD — native async
// ===========================================================================

#[cfg(not(feature = "wasm-component"))]
pub async fn get(ctx: &dyn Context, collection: &str, id: &str) -> Result<Record, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::DATABASE_GET,
        &GetReq { collection, id },
    )
    .await?;
    decode(&data)
}

#[cfg(not(feature = "wasm-component"))]
pub async fn list(
    ctx: &dyn Context,
    collection: &str,
    opts: &ListOptions,
) -> Result<RecordList, WaferError> {
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
    )
    .await?;
    decode(&data)
}

#[cfg(not(feature = "wasm-component"))]
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
    )
    .await?;
    decode(&resp)
}

#[cfg(not(feature = "wasm-component"))]
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
    )
    .await?;
    decode(&resp)
}

#[cfg(not(feature = "wasm-component"))]
pub async fn delete(ctx: &dyn Context, collection: &str, id: &str) -> Result<(), WaferError> {
    call_service(
        ctx,
        BLOCK,
        ServiceOp::DATABASE_DELETE,
        &DeleteReq { collection, id },
    )
    .await?;
    Ok(())
}

#[cfg(not(feature = "wasm-component"))]
pub async fn count(
    ctx: &dyn Context,
    collection: &str,
    filters: &[Filter],
) -> Result<i64, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::DATABASE_COUNT,
        &CountReq {
            collection,
            filters: to_filter_defs(filters),
        },
    )
    .await?;
    let resp: CountResp = decode(&data)?;
    Ok(resp.count)
}

#[cfg(not(feature = "wasm-component"))]
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
    )
    .await?;
    let resp: SumResp = decode(&data)?;
    Ok(resp.sum)
}

#[cfg(not(feature = "wasm-component"))]
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
    )
    .await?;
    decode(&data)
}

#[cfg(not(feature = "wasm-component"))]
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
    )
    .await?;
    let resp: ExecRawResp = decode(&data)?;
    Ok(resp.rows_affected)
}

// --- Public API: higher-level helpers (native) ---

#[cfg(not(feature = "wasm-component"))]
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
    )
    .await?;
    result
        .records
        .into_iter()
        .next()
        .ok_or_else(|| WaferError::new(ErrorCode::NOT_FOUND, "record not found"))
}

#[cfg(not(feature = "wasm-component"))]
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

#[cfg(not(feature = "wasm-component"))]
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
    )
    .await?;
    Ok(result.records)
}

#[cfg(not(feature = "wasm-component"))]
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
    )
    .await
}

#[cfg(not(feature = "wasm-component"))]
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

#[cfg(not(feature = "wasm-component"))]
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
    )
    .await?;
    for r in records {
        delete(ctx, collection, &r.id).await?;
    }
    Ok(())
}

#[cfg(not(feature = "wasm-component"))]
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
    )
    .await
}

#[cfg(not(feature = "wasm-component"))]
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

#[cfg(not(feature = "wasm-component"))]
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

// ===========================================================================
// Public API: core CRUD — WASM sync (calls WIT runtime::call-block import)
// ===========================================================================

#[cfg(feature = "wasm-component")]
pub fn get(collection: &str, id: &str) -> Result<Record, WaferError> {
    let data = call_service(BLOCK, ServiceOp::DATABASE_GET, &GetReq { collection, id })?;
    decode(&data)
}

#[cfg(feature = "wasm-component")]
pub fn list(collection: &str, opts: &ListOptions) -> Result<RecordList, WaferError> {
    let data = call_service(
        BLOCK,
        ServiceOp::DATABASE_LIST,
        &ListReq {
            collection,
            filters: to_filter_defs(&opts.filters),
            sort: to_sort_defs(&opts.sort),
            limit: opts.limit,
            offset: opts.offset,
        },
    )?;
    decode(&data)
}

#[cfg(feature = "wasm-component")]
pub fn create(
    collection: &str,
    data: HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    let resp = call_service(
        BLOCK,
        ServiceOp::DATABASE_CREATE,
        &CreateReq {
            collection,
            data: &data,
        },
    )?;
    decode(&resp)
}

#[cfg(feature = "wasm-component")]
pub fn update(
    collection: &str,
    id: &str,
    data: HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    let resp = call_service(
        BLOCK,
        ServiceOp::DATABASE_UPDATE,
        &UpdateReq {
            collection,
            id,
            data: &data,
        },
    )?;
    decode(&resp)
}

#[cfg(feature = "wasm-component")]
pub fn delete(collection: &str, id: &str) -> Result<(), WaferError> {
    call_service(
        BLOCK,
        ServiceOp::DATABASE_DELETE,
        &DeleteReq { collection, id },
    )?;
    Ok(())
}

#[cfg(feature = "wasm-component")]
pub fn count(collection: &str, filters: &[Filter]) -> Result<i64, WaferError> {
    let data = call_service(
        BLOCK,
        ServiceOp::DATABASE_COUNT,
        &CountReq {
            collection,
            filters: to_filter_defs(filters),
        },
    )?;
    let resp: CountResp = decode(&data)?;
    Ok(resp.count)
}

#[cfg(feature = "wasm-component")]
pub fn sum(collection: &str, field: &str, filters: &[Filter]) -> Result<f64, WaferError> {
    let data = call_service(
        BLOCK,
        ServiceOp::DATABASE_SUM,
        &SumReq {
            collection,
            field,
            filters: to_filter_defs(filters),
        },
    )?;
    let resp: SumResp = decode(&data)?;
    Ok(resp.sum)
}

#[cfg(feature = "wasm-component")]
pub fn query_raw(query: &str, args: &[serde_json::Value]) -> Result<Vec<Record>, WaferError> {
    let data = call_service(
        BLOCK,
        ServiceOp::DATABASE_QUERY_RAW,
        &QueryRawReq { query, args },
    )?;
    decode(&data)
}

#[cfg(feature = "wasm-component")]
pub fn exec_raw(query: &str, args: &[serde_json::Value]) -> Result<i64, WaferError> {
    let data = call_service(
        BLOCK,
        ServiceOp::DATABASE_EXEC_RAW,
        &ExecRawReq { query, args },
    )?;
    let resp: ExecRawResp = decode(&data)?;
    Ok(resp.rows_affected)
}

// --- Public API: higher-level helpers (WASM) ---

#[cfg(feature = "wasm-component")]
pub fn get_by_field(
    collection: &str,
    field: &str,
    value: serde_json::Value,
) -> Result<Record, WaferError> {
    let result = list(
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
    )?;
    result
        .records
        .into_iter()
        .next()
        .ok_or_else(|| WaferError::new(ErrorCode::NOT_FOUND, "record not found"))
}

#[cfg(feature = "wasm-component")]
pub fn upsert(
    collection: &str,
    field: &str,
    value: serde_json::Value,
    data: HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    match get_by_field(collection, field, value) {
        Ok(existing) => update(collection, &existing.id, data),
        Err(e) if e.code == ErrorCode::NOT_FOUND => create(collection, data),
        Err(e) => Err(e),
    }
}

#[cfg(feature = "wasm-component")]
pub fn list_all(collection: &str, filters: Vec<Filter>) -> Result<Vec<Record>, WaferError> {
    let result = list(
        collection,
        &ListOptions {
            filters,
            limit: 100000,
            ..Default::default()
        },
    )?;
    Ok(result.records)
}

#[cfg(feature = "wasm-component")]
pub fn paginated_list(
    collection: &str,
    page: i64,
    page_size: i64,
    filters: Vec<Filter>,
    sort: Vec<SortField>,
) -> Result<RecordList, WaferError> {
    let page = if page < 1 { 1 } else { page };
    let page_size = if page_size < 1 { 20 } else { page_size };
    list(
        collection,
        &ListOptions {
            filters,
            sort,
            limit: page_size,
            offset: (page - 1).saturating_mul(page_size),
        },
    )
}

#[cfg(feature = "wasm-component")]
pub fn soft_delete(collection: &str, id: &str) -> Result<Record, WaferError> {
    let mut data = HashMap::new();
    data.insert(
        "deleted_at".to_string(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    update(collection, id, data)
}

#[cfg(feature = "wasm-component")]
pub fn delete_by_field(
    collection: &str,
    field: &str,
    value: serde_json::Value,
) -> Result<(), WaferError> {
    let records = list_all(
        collection,
        vec![Filter {
            field: field.to_string(),
            operator: FilterOp::Equal,
            value,
        }],
    )?;
    for r in records {
        delete(collection, &r.id)?;
    }
    Ok(())
}

#[cfg(feature = "wasm-component")]
pub fn count_by_field(
    collection: &str,
    field: &str,
    value: serde_json::Value,
) -> Result<i64, WaferError> {
    count(
        collection,
        &[Filter {
            field: field.to_string(),
            operator: FilterOp::Equal,
            value,
        }],
    )
}

#[cfg(feature = "wasm-component")]
pub fn delete_by_filters(collection: &str, filters: Vec<Filter>) -> Result<(), WaferError> {
    let records = list_all(collection, filters)?;
    for r in records {
        delete(collection, &r.id)?;
    }
    Ok(())
}

#[cfg(feature = "wasm-component")]
pub fn update_by_filters(
    collection: &str,
    filters: Vec<Filter>,
    data: HashMap<String, serde_json::Value>,
) -> Result<(), WaferError> {
    let records = list_all(collection, filters)?;
    for r in records {
        update(collection, &r.id, data.clone())?;
    }
    Ok(())
}
