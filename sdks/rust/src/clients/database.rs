//! Typed client for the database service.
//!
//! Every op in [`ServiceOp::DATABASE_OPS`] is wrapped here (enforced by the
//! `sdk_covers_every_database_op` test) as a buffered single-frame
//! request/response. Mutating ops that return no value (`delete`,
//! `delete_where`, `update_where`) yield an empty acknowledgement; the rest
//! decode a typed response.
//!
//! `query_raw` and `aggregate` are special: the host handler encodes the
//! result as a MessagePack-encoded `Vec<Record>` directly (one frame), so
//! the response type here is `Vec<Record>`.

use wafer_block::{
    wire::database::{
        AddColumnRequest, AggregateRequest, CountRequest, CountResponse, CreateRequest,
        DeleteRequest, DeleteWhereCountRequest, DeleteWhereCountResponse, DeleteWhereRequest,
        DropTableRequest, EnsureTableRequest, ExecRawRequest, ExecRawResponse, GetRequest,
        IncrementFieldWhereRequest, ListRequest, QueryRawRequest, Record, RecordList,
        SchemaOpResponse, SumRequest, SumResponse, TableExistsRequest, TableExistsResponse,
        TakeWhereRequest, TakeWhereResponse, UpdateRequest, UpdateWhereCountRequest,
        UpdateWhereCountResponse, UpdateWhereRequest, UpsertRequest, UpsertResponse,
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

/// Buffered: update all records matching the filters with `data` and return
/// the number of updated rows.
pub fn update_where_count(
    request: &UpdateWhereCountRequest,
) -> Result<UpdateWhereCountResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_UPDATE_WHERE_COUNT, request)
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

/// Buffered: atomically increment a numeric column on all records matching
/// the filters, returning the number of affected rows.
pub fn increment_field_where(
    request: &IncrementFieldWhereRequest,
) -> Result<ExecRawResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_INCREMENT_FIELD_WHERE, request)
}

/// Buffered: insert a row, resolving a conflict on the request's
/// `conflict_columns` via its `on_conflict` strategy (`SetColumns` or
/// `WindowedCounter`), in one atomic `INSERT … ON CONFLICT …` round-trip.
pub fn upsert(request: &UpsertRequest) -> Result<UpsertResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_UPSERT, request)
}

/// Buffered: run a grouped aggregate query and return one [`Record`] per
/// group. Like `query_raw`, the handler MessagePack-encodes `Vec<Record>`
/// directly into a single response frame.
pub fn aggregate(request: &AggregateRequest) -> Result<Vec<Record>, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_AGGREGATE, request)
}

/// Buffered: execute a DDL statement (`CREATE TABLE`, `CREATE INDEX`, …),
/// returning the number of affected rows. Routes through the permissive
/// `__ddl__` WRAP resource — blocks DDL their own `{org}__{block}__*`
/// tables at init.
pub fn ddl(request: &ExecRawRequest) -> Result<ExecRawResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_DDL, request)
}

/// Buffered: create a table and its indexes if absent (structured DDL,
/// authorized on the table name and `__ddl__`).
pub fn ensure_table(request: &EnsureTableRequest) -> Result<SchemaOpResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_ENSURE_TABLE, request)
}
/// Buffered: add one column to a table.
pub fn add_column(request: &AddColumnRequest) -> Result<SchemaOpResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_ADD_COLUMN, request)
}
/// Buffered: drop a table if present.
pub fn drop_table(request: &DropTableRequest) -> Result<SchemaOpResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_DROP_TABLE, request)
}
/// Buffered: whether a table exists.
pub fn table_exists(request: &TableExistsRequest) -> Result<TableExistsResponse, WaferError> {
    call(BLOCK, ServiceOp::DATABASE_TABLE_EXISTS, request)
}

/// Every database op this client wraps. Kept set-equal to
/// [`ServiceOp::DATABASE_OPS`] by the `sdk_covers_every_database_op` test —
/// adding an op to the family without adding a wrapper (and an entry here)
/// fails that test.
const SUPPORTED_DATABASE_OPS: &[&str] = &[
    ServiceOp::DATABASE_GET,
    ServiceOp::DATABASE_LIST,
    ServiceOp::DATABASE_CREATE,
    ServiceOp::DATABASE_UPDATE,
    ServiceOp::DATABASE_UPDATE_WHERE,
    ServiceOp::DATABASE_UPDATE_WHERE_COUNT,
    ServiceOp::DATABASE_DELETE,
    ServiceOp::DATABASE_DELETE_WHERE,
    ServiceOp::DATABASE_DELETE_WHERE_COUNT,
    ServiceOp::DATABASE_TAKE_WHERE,
    ServiceOp::DATABASE_COUNT,
    ServiceOp::DATABASE_SUM,
    ServiceOp::DATABASE_AGGREGATE,
    ServiceOp::DATABASE_INCREMENT_FIELD_WHERE,
    ServiceOp::DATABASE_UPSERT,
    ServiceOp::DATABASE_QUERY_RAW,
    ServiceOp::DATABASE_EXEC_RAW,
    ServiceOp::DATABASE_DDL,
    ServiceOp::DATABASE_ENSURE_TABLE,
    ServiceOp::DATABASE_ADD_COLUMN,
    ServiceOp::DATABASE_DROP_TABLE,
    ServiceOp::DATABASE_TABLE_EXISTS,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// SDK-drift guard: the typed client must wrap every op in the
    /// `database.*` family. This is the test that was missing when
    /// `DATABASE_UPDATE_WHERE_COUNT`, `DATABASE_AGGREGATE`,
    /// `DATABASE_INCREMENT_FIELD_WHERE`, `DATABASE_UPSERT` and
    /// `DATABASE_DDL` landed host-side without SDK wrappers (SP-B1/SP-B2
    /// follow-up).
    #[test]
    fn sdk_covers_every_database_op() {
        let mut family: Vec<&str> = ServiceOp::DATABASE_OPS.to_vec();
        let mut supported: Vec<&str> = SUPPORTED_DATABASE_OPS.to_vec();
        family.sort_unstable();
        supported.sort_unstable();
        assert_eq!(
            family, supported,
            "sdks/rust database client is out of sync with ServiceOp::DATABASE_OPS — \
             add a wrapper fn and a SUPPORTED_DATABASE_OPS entry for each missing op"
        );
    }
}
