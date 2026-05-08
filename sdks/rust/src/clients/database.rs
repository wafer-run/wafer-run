//! Typed client for the database service.
//!
//! All eleven ops are buffered single-frame request/response. Mutating ops
//! that return no value (`delete`, `delete_where`, `update_where`) yield
//! an empty acknowledgement; the rest decode a typed response.
//!
//! `query_raw` is special: the host handler encodes the result as a
//! MessagePack-encoded `Vec<Record>` directly (one frame), so the response
//! type here is `Vec<Record>`.

use wafer_block::{
    codec,
    wire::database::{
        CountRequest, CountResponse, CreateRequest, DeleteRequest, DeleteWhereCountRequest,
        DeleteWhereCountResponse, DeleteWhereRequest, ExecRawRequest, ExecRawResponse, GetRequest,
        ListRequest, QueryRawRequest, Record, RecordList, SumRequest, SumResponse,
        TakeWhereRequest, TakeWhereResponse, UpdateRequest, UpdateWhereRequest,
    },
    ServiceOp, WaferError,
};

use super::common::{collect_single_frame, consume_ack, open_buffered};

const BLOCK: &str = "wafer-run/database";

/// Buffered: fetch a single record by primary key.
pub fn get(request: &GetRequest) -> Result<Record, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::DATABASE_GET, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "database GET")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding database GET response: {}", e.message),
        )
    })
}

/// Buffered: list records matching the request's filters / sort /
/// pagination. Returns the full [`RecordList`].
pub fn list(request: &ListRequest) -> Result<RecordList, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::DATABASE_LIST, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "database LIST")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding database LIST response: {}", e.message),
        )
    })
}

/// Buffered: create a new record. Returns the persisted [`Record`]
/// (including any server-generated id / timestamps).
pub fn create(request: &CreateRequest) -> Result<Record, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::DATABASE_CREATE, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "database CREATE")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding database CREATE response: {}", e.message),
        )
    })
}

/// Buffered: update a record by id. Returns the updated [`Record`].
pub fn update(request: &UpdateRequest) -> Result<Record, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::DATABASE_UPDATE, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "database UPDATE")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding database UPDATE response: {}", e.message),
        )
    })
}

/// Buffered: delete a record by id. The response is an empty
/// acknowledgement.
pub fn delete(request: &DeleteRequest) -> Result<(), WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::DATABASE_DELETE, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: count records matching the request's filters.
pub fn count(request: &CountRequest) -> Result<CountResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::DATABASE_COUNT, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "database COUNT")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding database COUNT response: {}", e.message),
        )
    })
}

/// Buffered: sum a numeric field across records matching the filters.
pub fn sum(request: &SumRequest) -> Result<SumResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::DATABASE_SUM, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "database SUM")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding database SUM response: {}", e.message),
        )
    })
}

/// Buffered: run a raw SELECT-style query and return a vector of
/// [`Record`]s. The handler MessagePack-encodes `Vec<Record>` directly into
/// a single response frame.
pub fn query_raw(request: &QueryRawRequest) -> Result<Vec<Record>, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::DATABASE_QUERY_RAW, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "database QUERY_RAW")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding database QUERY_RAW response: {}", e.message),
        )
    })
}

/// Buffered: run a raw mutation (INSERT/UPDATE/DELETE), returning the
/// number of affected rows.
pub fn exec_raw(request: &ExecRawRequest) -> Result<ExecRawResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::DATABASE_EXEC_RAW, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "database EXEC_RAW")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding database EXEC_RAW response: {}", e.message),
        )
    })
}

/// Buffered: delete all records matching the filters. The response is an
/// empty acknowledgement.
pub fn delete_where(request: &DeleteWhereRequest) -> Result<(), WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::DATABASE_DELETE_WHERE, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: update all records matching the filters with `data`. The
/// response is an empty acknowledgement (the host handler returns `()`,
/// not the affected records).
pub fn update_where(request: &UpdateWhereRequest) -> Result<(), WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::DATABASE_UPDATE_WHERE, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: delete all records matching the filters and return the number of
/// deleted rows.
pub fn delete_where_count(
    request: &DeleteWhereCountRequest,
) -> Result<DeleteWhereCountResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream =
        open_buffered(BLOCK, ServiceOp::DATABASE_DELETE_WHERE_COUNT, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "database DELETE_WHERE_COUNT")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!(
                "decoding database DELETE_WHERE_COUNT response: {}",
                e.message
            ),
        )
    })
}

/// Buffered: atomically select and delete all records matching the filters,
/// returning the deleted rows.
pub fn take_where(request: &TakeWhereRequest) -> Result<TakeWhereResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::DATABASE_TAKE_WHERE, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "database TAKE_WHERE")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding database TAKE_WHERE response: {}", e.message),
        )
    })
}
