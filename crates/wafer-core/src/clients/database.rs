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

use super::{call_service, decode, dual_api, svc, svc_fn};

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
struct DeleteWhereReq<'a> {
    collection: &'a str,
    filters: Vec<FilterDef<'a>>,
}

#[derive(Serialize)]
struct UpdateWhereReq<'a> {
    collection: &'a str,
    filters: Vec<FilterDef<'a>>,
    data: &'a HashMap<String, serde_json::Value>,
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
// Public API — generated as async (native) or sync (wasm-component)
// ===========================================================================

dual_api! {
    // --- Core CRUD ---

    pub fn get(ctx, collection: &str, id: &str) -> Result<Record, WaferError> {
        let data = svc!(ctx, BLOCK, ServiceOp::DATABASE_GET, &GetReq { collection, id }, Some(collection), false, Some("db"))?;
        decode(&data)
    }

    pub fn list(ctx, collection: &str, opts: &ListOptions) -> Result<RecordList, WaferError> {
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_LIST,
            &ListReq {
                collection,
                filters: to_filter_defs(&opts.filters),
                sort: to_sort_defs(&opts.sort),
                limit: opts.limit,
                offset: opts.offset,
            },
            Some(collection),
            false,
            Some("db")
        )?;
        decode(&data)
    }

    pub fn create(ctx, collection: &str, data: HashMap<String, serde_json::Value>) -> Result<Record, WaferError> {
        let resp = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_CREATE,
            &CreateReq { collection, data: &data },
            Some(collection),
            true,
            Some("db")
        )?;
        decode(&resp)
    }

    pub fn update(ctx, collection: &str, id: &str, data: HashMap<String, serde_json::Value>) -> Result<Record, WaferError> {
        let resp = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_UPDATE,
            &UpdateReq { collection, id, data: &data },
            Some(collection),
            true,
            Some("db")
        )?;
        decode(&resp)
    }

    pub fn delete(ctx, collection: &str, id: &str) -> Result<(), WaferError> {
        svc!(ctx, BLOCK, ServiceOp::DATABASE_DELETE, &DeleteReq { collection, id }, Some(collection), true, Some("db"))?;
        Ok(())
    }

    pub fn count(ctx, collection: &str, filters: &[Filter]) -> Result<i64, WaferError> {
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_COUNT,
            &CountReq { collection, filters: to_filter_defs(filters) },
            Some(collection),
            false,
            Some("db")
        )?;
        let resp: CountResp = decode(&data)?;
        Ok(resp.count)
    }

    pub fn sum(ctx, collection: &str, field: &str, filters: &[Filter]) -> Result<f64, WaferError> {
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_SUM,
            &SumReq { collection, field, filters: to_filter_defs(filters) },
            Some(collection),
            false,
            Some("db")
        )?;
        let resp: SumResp = decode(&data)?;
        Ok(resp.sum)
    }

    pub fn query_raw(ctx, query: &str, args: &[serde_json::Value]) -> Result<Vec<Record>, WaferError> {
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_QUERY_RAW,
            &QueryRawReq { query, args },
            Some("__raw_sql__"),
            false,
            Some("db")
        )?;
        decode(&data)
    }

    pub fn exec_raw(ctx, query: &str, args: &[serde_json::Value]) -> Result<i64, WaferError> {
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_EXEC_RAW,
            &ExecRawReq { query, args },
            Some("__raw_sql__"),
            true,
            Some("db")
        )?;
        let resp: ExecRawResp = decode(&data)?;
        Ok(resp.rows_affected)
    }

    // --- Higher-level helpers ---

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
            .ok_or_else(|| WaferError::new(ErrorCode::NOT_FOUND, "record not found"))
    }

    pub fn upsert(
        ctx,
        collection: &str,
        field: &str,
        value: serde_json::Value,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, WaferError> {
        match svc_fn!(ctx, get_by_field(collection, field, value)) {
            Ok(existing) => svc_fn!(ctx, update(collection, &existing.id, data)),
            Err(e) if e.code == ErrorCode::NOT_FOUND => svc_fn!(ctx, create(collection, data)),
            Err(e) => Err(e),
        }
    }

    /// List all records matching the given filters.
    ///
    /// Intended for small, bounded collections (roles, permissions, legal docs).
    /// Hard-capped at 10,000 records — use paginated `list()` for larger collections.
    pub fn list_all(ctx, collection: &str, filters: Vec<Filter>) -> Result<Vec<Record>, WaferError> {
        let result = svc_fn!(ctx, list(
            collection,
            &ListOptions {
                filters,
                limit: 10_000,
                ..Default::default()
            }
        ))?;
        Ok(result.records)
    }

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
            }
        ))
    }

    pub fn soft_delete(ctx, collection: &str, id: &str) -> Result<Record, WaferError> {
        let mut data = HashMap::new();
        data.insert(
            "deleted_at".to_string(),
            serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
        );
        svc_fn!(ctx, update(collection, id, data))
    }

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

    pub fn delete_by_filters(ctx, collection: &str, filters: Vec<Filter>) -> Result<(), WaferError> {
        svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_DELETE_WHERE,
            &DeleteWhereReq { collection, filters: to_filter_defs(&filters) },
            Some(collection),
            true,
            Some("db")
        )?;
        Ok(())
    }

    pub fn update_by_filters(
        ctx,
        collection: &str,
        filters: Vec<Filter>,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), WaferError> {
        svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_UPDATE_WHERE,
            &UpdateWhereReq { collection, filters: to_filter_defs(&filters), data: &data },
            Some(collection),
            true,
            Some("db")
        )?;
        Ok(())
    }
}
