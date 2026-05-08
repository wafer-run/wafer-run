use std::collections::HashMap;

#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
// `Record` and `RecordList` are byte-identical to the wire types; collapse
// the duplicate by re-exporting from the wire crate.
pub use wafer_block::wire::database::{Record, RecordList};
use wafer_block::{
    common::{ErrorCode, ServiceOp},
    wire::database::{
        CountRequest, CountResponse, CreateRequest, DeleteRequest, DeleteWhereCountRequest,
        DeleteWhereCountResponse, DeleteWhereRequest, ExecRawRequest, ExecRawResponse,
        FilterDef as WireFilterDef, GetRequest, ListRequest, QueryRawRequest,
        SortFieldDef as WireSortFieldDef, SumRequest, SumResponse, TakeWhereRequest,
        TakeWhereResponse, UpdateRequest, UpdateWhereRequest,
    },
    WaferError,
};

use super::{call_service, decode, dual_api, svc, svc_fn};
// Re-export schema types for declarative table management.
pub use crate::interfaces::database::service::{
    col_blob, col_bool, col_datetime, col_float, col_int, col_int64, col_json, col_string,
    col_text, default_empty, default_false, default_int, default_now, default_null, default_string,
    default_true, default_zero, pk, pk_int, schema_soft_delete, timestamps, Column, DataType,
    DefaultVal, DefaultValue, Index, Reference, Table,
};
// `Filter`, `FilterOp`, `ListOptions`, `SortField` are runtime-only ergonomic
// wrappers (no wire-format derives); keep them on the interfaces type and
// convert to `wire::*Def` at the wire boundary inside each fn.
pub use crate::interfaces::database::service::{Filter, FilterOp, ListOptions, SortField};

const BLOCK: &str = "wafer-run/database";

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

fn to_wire_filters(filters: &[Filter]) -> Vec<WireFilterDef> {
    filters
        .iter()
        .map(|f| WireFilterDef {
            field: f.field.clone(),
            operator: filter_op_str(&f.operator).to_string(),
            value: f.value.clone(),
        })
        .collect()
}

fn to_wire_sort(sort: &[SortField]) -> Vec<WireSortFieldDef> {
    sort.iter()
        .map(|s| WireSortFieldDef {
            field: s.field.clone(),
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
        let req = GetRequest { collection: collection.to_string(), id: id.to_string() };
        let data = svc!(ctx, BLOCK, ServiceOp::DATABASE_GET, &req, Some(collection), false, Some("db"))?;
        decode(&data)
    }

    pub fn list(ctx, collection: &str, opts: &ListOptions) -> Result<RecordList, WaferError> {
        let req = ListRequest {
            collection: collection.to_string(),
            filters: to_wire_filters(&opts.filters),
            sort: to_wire_sort(&opts.sort),
            limit: opts.limit,
            offset: opts.offset,
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_LIST,
            &req,
            Some(collection),
            false,
            Some("db")
        )?;
        decode(&data)
    }

    pub fn create(ctx, collection: &str, data: HashMap<String, serde_json::Value>) -> Result<Record, WaferError> {
        let req = CreateRequest { collection: collection.to_string(), data };
        let resp = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_CREATE,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        decode(&resp)
    }

    pub fn update(ctx, collection: &str, id: &str, data: HashMap<String, serde_json::Value>) -> Result<Record, WaferError> {
        let req = UpdateRequest { collection: collection.to_string(), id: id.to_string(), data };
        let resp = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_UPDATE,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        decode(&resp)
    }

    pub fn delete(ctx, collection: &str, id: &str) -> Result<(), WaferError> {
        let req = DeleteRequest { collection: collection.to_string(), id: id.to_string() };
        svc!(ctx, BLOCK, ServiceOp::DATABASE_DELETE, &req, Some(collection), true, Some("db"))?;
        Ok(())
    }

    pub fn count(ctx, collection: &str, filters: &[Filter]) -> Result<i64, WaferError> {
        let req = CountRequest {
            collection: collection.to_string(),
            filters: to_wire_filters(filters),
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_COUNT,
            &req,
            Some(collection),
            false,
            Some("db")
        )?;
        let resp: CountResponse = decode(&data)?;
        Ok(resp.count)
    }

    pub fn sum(ctx, collection: &str, field: &str, filters: &[Filter]) -> Result<f64, WaferError> {
        let req = SumRequest {
            collection: collection.to_string(),
            field: field.to_string(),
            filters: to_wire_filters(filters),
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_SUM,
            &req,
            Some(collection),
            false,
            Some("db")
        )?;
        let resp: SumResponse = decode(&data)?;
        Ok(resp.sum)
    }

    pub fn query_raw(ctx, query: &str, args: &[serde_json::Value]) -> Result<Vec<Record>, WaferError> {
        let req = QueryRawRequest { query: query.to_string(), args: args.to_vec() };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_QUERY_RAW,
            &req,
            Some("__raw_sql__"),
            false,
            Some("db")
        )?;
        decode(&data)
    }

    pub fn exec_raw(ctx, query: &str, args: &[serde_json::Value]) -> Result<i64, WaferError> {
        let req = ExecRawRequest { query: query.to_string(), args: args.to_vec() };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_EXEC_RAW,
            &req,
            Some("__raw_sql__"),
            true,
            Some("db")
        )?;
        let resp: ExecRawResponse = decode(&data)?;
        Ok(resp.rows_affected)
    }

    /// Execute a DDL statement (`CREATE TABLE`, `CREATE INDEX`, `DROP TABLE`, etc).
    ///
    /// DDL routes through the `__ddl__` WRAP resource, which is permissive: any
    /// attributable caller can DDL (no admin required). This is intentional —
    /// each block is expected to DDL its own (`{org}__{block}__*`) tables on
    /// init via `migrations::apply` or equivalent, without needing the admin
    /// block in the loop.
    ///
    /// Convention (NOT enforced by parsing SQL here): blocks only DDL their own
    /// tables. Cross-block DDL through this entry point is a misuse caught by
    /// code review and the `scripts/audit-wrap-grants.sh` audit script.
    ///
    /// Use `exec_raw` for raw DML/DDL that legitimately requires the admin
    /// block (the SQL explorer, ad-hoc operator queries). `exec_raw` stays
    /// admin-gated under WRAP.
    pub fn ddl(ctx, statement: &str) -> Result<i64, WaferError> {
        let req = ExecRawRequest { query: statement.to_string(), args: vec![] };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_EXEC_RAW,
            &req,
            Some("__ddl__"),
            true,
            Some("db")
        )?;
        let resp: ExecRawResponse = decode(&data)?;
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
        let req = DeleteWhereRequest {
            collection: collection.to_string(),
            filters: to_wire_filters(&filters),
        };
        svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_DELETE_WHERE,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        Ok(())
    }

    /// Delete all records matching the filters and return the number of deleted rows.
    pub fn delete_by_filters_count(ctx, collection: &str, filters: Vec<Filter>) -> Result<i64, WaferError> {
        let req = DeleteWhereCountRequest {
            collection: collection.to_string(),
            filters: to_wire_filters(&filters),
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_DELETE_WHERE_COUNT,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        let resp: DeleteWhereCountResponse = decode(&data)?;
        Ok(resp.count)
    }

    /// Atomically select and delete all records matching the filters, returning the deleted rows.
    pub fn take_by_filters(ctx, collection: &str, filters: Vec<Filter>) -> Result<Vec<Record>, WaferError> {
        let req = TakeWhereRequest {
            collection: collection.to_string(),
            filters: to_wire_filters(&filters),
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_TAKE_WHERE,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        let resp: TakeWhereResponse = decode(&data)?;
        Ok(resp.records)
    }

    pub fn update_by_filters(
        ctx,
        collection: &str,
        filters: Vec<Filter>,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), WaferError> {
        let req = UpdateWhereRequest {
            collection: collection.to_string(),
            filters: to_wire_filters(&filters),
            data,
        };
        svc!(
            ctx, BLOCK,
            ServiceOp::DATABASE_UPDATE_WHERE,
            &req,
            Some(collection),
            true,
            Some("db")
        )?;
        Ok(())
    }
}
