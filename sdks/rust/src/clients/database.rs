//! Typed client for the database service.
//!
//! All thirteen ops are buffered single-frame request/response. Mutating
//! ops that return no value (`delete`, `delete_where`, `update_where`)
//! yield an empty acknowledgement; the rest decode a typed response.
//!
//! `query_raw` is special: the host handler encodes the result as a
//! MessagePack-encoded `Vec<Record>` directly (one frame), so the response
//! type here is `Vec<Record>`.

use wafer_block::{
    wire::database::{
        CountRequest, CountResponse, CreateRequest, DeleteRequest, DeleteWhereCountRequest,
        DeleteWhereCountResponse, DeleteWhereRequest, ExecRawRequest, ExecRawResponse, GetRequest,
        ListRequest, QueryRawRequest, Record, RecordList, SumRequest, SumResponse,
        TakeWhereRequest, TakeWhereResponse, UpdateRequest, UpdateWhereRequest,
    },
    ServiceOp, WaferError,
};

use super::common::{call, call_ack};

const BLOCK: &str = "wafer-run/database";

/// Buffered: fetch a single record by primary key.
pub fn get(request: &GetRequest) -> Result<Record, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_GET, request)
}

/// Buffered: list records matching the request's filters / sort /
/// pagination. Returns the full [`RecordList`].
pub fn list(request: &ListRequest) -> Result<RecordList, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_LIST, request)
}

/// Buffered: create a new record. Returns the persisted [`Record`]
/// (including any server-generated id / timestamps).
pub fn create(request: &CreateRequest) -> Result<Record, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_CREATE, request)
}

/// Buffered: update a record by id. Returns the updated [`Record`].
pub fn update(request: &UpdateRequest) -> Result<Record, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_UPDATE, request)
}

/// Buffered: delete a record by id. The response is an empty
/// acknowledgement.
pub fn delete(request: &DeleteRequest) -> Result<(), WaferError> {
    call_ack(BLOCK, ServiceOp::DATABASE_DELETE, request)
}

/// Buffered: count records matching the request's filters.
pub fn count(request: &CountRequest) -> Result<CountResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_COUNT, request)
}

/// Buffered: sum a numeric field across records matching the filters.
pub fn sum(request: &SumRequest) -> Result<SumResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_SUM, request)
}

/// Buffered: run a raw SELECT-style query and return a vector of
/// [`Record`]s. The handler MessagePack-encodes `Vec<Record>` directly into
/// a single response frame.
pub fn query_raw(request: &QueryRawRequest) -> Result<Vec<Record>, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_QUERY_RAW, request)
}

/// Buffered: run a raw mutation (INSERT/UPDATE/DELETE), returning the
/// number of affected rows.
pub fn exec_raw(request: &ExecRawRequest) -> Result<ExecRawResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_EXEC_RAW, request)
}

/// Buffered: delete all records matching the filters. The response is an
/// empty acknowledgement.
pub fn delete_where(request: &DeleteWhereRequest) -> Result<(), WaferError> {
    call_ack(BLOCK, ServiceOp::DATABASE_DELETE_WHERE, request)
}

/// Buffered: update all records matching the filters with `data`. The
/// response is an empty acknowledgement (the host handler returns `()`,
/// not the affected records).
pub fn update_where(request: &UpdateWhereRequest) -> Result<(), WaferError> {
    call_ack(BLOCK, ServiceOp::DATABASE_UPDATE_WHERE, request)
}

/// Buffered: delete all records matching the filters and return the number of
/// deleted rows.
pub fn delete_where_count(
    request: &DeleteWhereCountRequest,
) -> Result<DeleteWhereCountResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_DELETE_WHERE_COUNT, request)
}

/// Buffered: atomically select and delete all records matching the filters,
/// returning the deleted rows.
pub fn take_where(request: &TakeWhereRequest) -> Result<TakeWhereResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_TAKE_WHERE, request)
}
