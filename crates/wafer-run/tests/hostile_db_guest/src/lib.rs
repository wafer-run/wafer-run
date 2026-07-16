//! Task 8 hostile-guest e2e fixture (SP-A Stage 1 + SP-B1 structured ops).
//!
//! Implements the standard `__wafer_handle(ptr, len) -> i64` ABI directly
//! (without the `#[wafer_block]` macro) so the fixture stays self-contained,
//! matching `dispatch_guest`/`service_client_guest`. Operations are selected
//! by `msg.kind`:
//!
//! ## SP-A raw-SQL vectors (admin-only, always denied for this guest)
//!   - `"test.exec_raw_evil"` — `exec_raw` `CREATE TABLE` for a namespace this
//!     guest does not own, via the ordinary public SDK client (no WRAP meta).
//!   - `"test.query_raw_secrets"` — same shape, `query_raw` reading a foreign
//!     collection.
//!
//! ## SP-B1 structured ops (typed `DATABASE_UPSERT` / `DATABASE_AGGREGATE`)
//! The public SDK has no typed `upsert`/`aggregate` client yet, so these emit
//! the wire request through the low-level streaming ABI (`CallStream`) — the
//! same transport the typed clients use — with no WRAP meta.
//!   - `"test.upsert_foreign"` — `DATABASE_UPSERT` naming a collection in a
//!     namespace this guest does not own (collection B). Must be denied
//!     server-side; the write must never reach B.
//!   - `"test.aggregate_foreign"` — `DATABASE_AGGREGATE` naming collection B.
//!     Must be denied; the read must never reach B's rows.
//!   - `"test.aggregate_deep_filter"` — `DATABASE_AGGREGATE` on the guest's OWN
//!     collection A (authorized), carrying a `FilterNode` tree nested past the
//!     depth bound. Authorization passes; the malformed IR must be rejected as
//!     `InvalidArgument` before any SQL runs.
//!   - `"test.aggregate_empty_casewhen"` — `DATABASE_AGGREGATE` on collection A
//!     with a `CaseWhenSum` whose `when` predicate is empty. Same: authorized,
//!     then `InvalidArgument` before execution.
//!
//! None of this is a "bypass" — it's what an ordinary, unprivileged, honestly
//! written wasm block looks like when built against the public Rust SDK. That
//! is precisely the meta-omission vector SP-A closes and the trust boundary
//! SP-B1's server-side rendering enforces: authorization and IR validation must
//! not depend on the guest having set anything.

use wafer_sdk::{
    clients::database,
    codec,
    core_abi::{pack_ptr_len, GuestResult},
    stream::CallStream,
    wire::database::{
        AggregateColumnDef, AggregateRequest, ExecRawRequest, FilterDef, FilterNode, OnConflict,
        QueryRawRequest, SortFieldDef, UpsertRequest,
    },
    BlockInfo, ErrorCode, Message, ServiceOp, WaferError,
};

/// The database service block the guest dispatches structured ops to.
const DATABASE_BLOCK: &str = "wafer-run/database";

/// Collection the guest OWNS: its namespace prefix `test__hostile_db_guest__`
/// is derived from the block id `test/hostile-db-guest`, so WRAP Rule 3
/// (caller owns the resource) authorizes access without an explicit grant. A
/// well-formed op on this collection is therefore authorized — which is what
/// lets the malformed-IR cases get *past* authorization and be rejected by the
/// handler's IR validation (`InvalidArgument`) rather than short-circuiting at
/// `PermissionDenied`.
const OWN_COLLECTION: &str = "test__hostile_db_guest__counters";

/// A collection in a namespace the guest does NOT own (collection B). The e2e
/// test creates and seeds this table directly (bypassing WRAP, as a test oracle
/// legitimately may) so it can prove the denied op left B untouched.
const FOREIGN_COLLECTION: &str = "victim_org__victim_block__balances";

// `__wafer_alloc` is exported by `wafer_sdk::core_abi` on `wasm32-*` targets,
// so we don't redefine it here.

/// Block metadata export. Returns a JSON-encoded `BlockInfo` packed as
/// `(ptr << 32) | len` — the format `WasmiBlock::info()` expects.
#[no_mangle]
pub extern "C" fn __wafer_info() -> i64 {
    let info = BlockInfo::new(
        "test/hostile-db-guest",
        "0.0.0",
        "handler@v1",
        "Task 8 e2e fixture — an ordinary SDK guest with no WRAP grants",
    )
    // SEC-02: an "honest but unprivileged" guest still declares what it
    // legitimately uses: it calls the database block and operates on its
    // OWN collection. Foreign-namespace / raw-SQL attempts remain denied by
    // WRAP attribution (step 1 of check_resource_access), not by a missing
    // capability — so the PermissionDenied regression tests keep their
    // meaning, while the InvalidArgument tests (own-collection aggregates)
    // now reach the query validator instead of being capability-denied.
    .capabilities(wafer_sdk::BlockCapabilities {
        callable_blocks: wafer_sdk::Allowlist::Only([DATABASE_BLOCK].into_iter().map(String::from).collect()),
        collections: wafer_sdk::Allowlist::Only([OWN_COLLECTION].into_iter().map(String::from).collect()),
        ..wafer_sdk::BlockCapabilities::none()
    });
    let bytes = serde_json::to_vec(&info).expect("BlockInfo is JSON-serialisable");
    let ptr = bytes.as_ptr() as u32;
    let len = bytes.len() as u32;
    std::mem::forget(bytes);
    pack_ptr_len(ptr, len)
}

/// Lifecycle hook — no-op for this fixture.
#[no_mangle]
pub extern "C" fn __wafer_lifecycle(_evt_ptr: i32, _evt_len: i32) -> i64 {
    let bytes = serde_json::to_vec(&Ok::<(), WaferError>(()))
        .expect("Result<(), WaferError>::Ok(()) is JSON-serialisable");
    let ptr = bytes.as_ptr() as u32;
    let len = bytes.len() as u32;
    std::mem::forget(bytes);
    pack_ptr_len(ptr, len)
}

/// Standard wafer block handler entry point. Decodes the host-supplied
/// `(Message, Vec<u8>)` JSON tuple, dispatches on `msg.kind`, and returns a
/// JSON-encoded `GuestResult`.
#[no_mangle]
pub extern "C" fn __wafer_handle(msg_ptr: i32, msg_len: i32) -> i64 {
    let msg_bytes = unsafe { std::slice::from_raw_parts(msg_ptr as *const u8, msg_len as usize) };
    let result = match serde_json::from_slice::<(Message, Vec<u8>)>(msg_bytes) {
        Ok((msg, body)) => dispatch(&msg, &body),
        Err(e) => GuestResult::error(WaferError::new(
            ErrorCode::InvalidArgument,
            format!("hostile-db-guest: invalid (Message, body) tuple: {e}"),
        )),
    };
    let result_bytes =
        serde_json::to_vec(&result).expect("GuestResult is always JSON-serialisable");
    let ptr = result_bytes.as_ptr() as u32;
    let len = result_bytes.len() as u32;
    std::mem::forget(result_bytes);
    pack_ptr_len(ptr, len)
}

/// Emit an encoded structured request to the database block over the low-level
/// streaming ABI (no typed client, no WRAP meta) and return the single response
/// frame. A host-side denial or IR rejection surfaces as `Err(WaferError)` from
/// `next_chunk`, which propagates here unchanged.
fn call_db(op: &str, req_bytes: &[u8]) -> Result<Vec<u8>, WaferError> {
    let msg = Message::new(op);
    let mut call = CallStream::open(DATABASE_BLOCK, &msg)?;
    call.write_chunk(req_bytes)?;
    let mut response = call.finish()?;
    Ok(response.next_chunk()?.unwrap_or_default())
}

/// Dispatch an already-encoded structured request to `op` and fold the outcome
/// into a `GuestResult` (success → `respond`; host error → `error`, so the
/// denial/`InvalidArgument` surfaces to the outer test as a terminal error).
///
/// Takes the codec result so the concrete-typed `codec::encode(&req)` stays at
/// each call site (this fixture doesn't depend on `serde` directly, only on the
/// SDK's `codec`).
fn dispatch_structured(op: &str, encoded: Result<Vec<u8>, WaferError>) -> GuestResult {
    let bytes = match encoded {
        Ok(b) => b,
        Err(e) => return GuestResult::error(e),
    };
    match call_db(op, &bytes) {
        Ok(resp) => GuestResult::respond(resp),
        Err(e) => GuestResult::error(e),
    }
}

fn dispatch(msg: &Message, _body: &[u8]) -> GuestResult {
    match msg.kind.as_str() {
        "test.exec_raw_evil" => {
            let req = ExecRawRequest {
                query: "CREATE TABLE test_org__hostile_guest__evil (id TEXT)".to_string(),
                args: vec![],
            };
            match database::exec_raw(&req) {
                Ok(resp) => GuestResult::respond(
                    serde_json::to_vec(&resp).expect("ExecRawResponse is JSON-serialisable"),
                ),
                Err(e) => GuestResult::error(e),
            }
        }
        "test.query_raw_secrets" => {
            let req = QueryRawRequest {
                query: "SELECT * FROM my_org__other_block__secrets".to_string(),
                args: vec![],
            };
            match database::query_raw(&req) {
                Ok(records) => GuestResult::respond(
                    serde_json::to_vec(&records).expect("Vec<Record> is JSON-serialisable"),
                ),
                Err(e) => GuestResult::error(e),
            }
        }
        // SP-B1: DATABASE_UPSERT naming a foreign collection. If the host
        // rendered SQL from caller-supplied text (the vector SP-B closes) this
        // would write `balance = 999` into collection B; server-side render +
        // WRAP-check on `collection` must deny it, leaving B untouched.
        "test.upsert_foreign" => {
            let req = UpsertRequest {
                collection: FOREIGN_COLLECTION.to_string(),
                data: vec![
                    ("id".to_string(), serde_json::json!("b1")),
                    ("balance".to_string(), serde_json::json!(999)),
                ],
                conflict_columns: vec!["id".to_string()],
                on_conflict: OnConflict::SetColumns(vec!["balance".to_string()]),
            };
            dispatch_structured(ServiceOp::DATABASE_UPSERT, codec::encode(&req))
        }
        // SP-B1: DATABASE_AGGREGATE naming a foreign collection. Must be denied
        // before the read reaches B's rows.
        "test.aggregate_foreign" => {
            let req = AggregateRequest {
                collection: FOREIGN_COLLECTION.to_string(),
                select_columns: vec![],
                aggregates: vec![AggregateColumnDef::Count {
                    alias: "n".to_string(),
                }],
                filters: vec![],
                group_by: vec![],
                sort: vec![],
                limit: 0,
            };
            dispatch_structured(ServiceOp::DATABASE_AGGREGATE, codec::encode(&req))
        }
        // SP-B1: DATABASE_AGGREGATE on the guest's OWN collection (authorized),
        // carrying a filter tree nested past the depth bound. Authorization
        // passes; `convert_filter_tree` must reject the malformed IR with
        // `InvalidArgument` before any SQL is built or run.
        "test.aggregate_deep_filter" => {
            let mut node = FilterNode::Leaf(FilterDef {
                field: "x".to_string(),
                operator: "eq".to_string(),
                value: serde_json::json!(1),
            });
            // 17 nested `all` groups puts the innermost node past
            // MAX_FILTER_DEPTH (16), the documented bound.
            for _ in 0..17 {
                node = FilterNode::All { all: vec![node] };
            }
            let req = AggregateRequest {
                collection: OWN_COLLECTION.to_string(),
                select_columns: vec![],
                aggregates: vec![AggregateColumnDef::Count {
                    alias: "n".to_string(),
                }],
                filters: vec![node],
                group_by: vec![],
                sort: vec![SortFieldDef {
                    field: "n".to_string(),
                    desc: true,
                }],
                limit: 0,
            };
            dispatch_structured(ServiceOp::DATABASE_AGGREGATE, codec::encode(&req))
        }
        // SP-B1: DATABASE_AGGREGATE on the OWN collection with a CaseWhenSum
        // whose `when` predicate is empty — a malformed aggregate IR. Rejected
        // as `InvalidArgument` after authorization, before execution.
        "test.aggregate_empty_casewhen" => {
            let req = AggregateRequest {
                collection: OWN_COLLECTION.to_string(),
                select_columns: vec![],
                aggregates: vec![AggregateColumnDef::CaseWhenSum {
                    when: vec![],
                    alias: "errs".to_string(),
                }],
                filters: vec![],
                group_by: vec![],
                sort: vec![],
                limit: 0,
            };
            dispatch_structured(ServiceOp::DATABASE_AGGREGATE, codec::encode(&req))
        }
        other => GuestResult::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("hostile-db-guest: unknown kind {other}"),
        )),
    }
}
