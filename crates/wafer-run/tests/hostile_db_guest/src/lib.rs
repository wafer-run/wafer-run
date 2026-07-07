//! Task 8 hostile-guest e2e fixture (SP-A Stage 1).
//!
//! Implements the standard `__wafer_handle(ptr, len) -> i64` ABI directly
//! (without the `#[wafer_block]` macro) so the fixture stays self-contained,
//! matching `dispatch_guest`/`service_client_guest`. Two operations,
//! selected by `msg.kind`:
//!
//!   - `"test.exec_raw_evil"` — calls `wafer_sdk::clients::database::exec_raw`
//!     with a `CREATE TABLE` statement for a namespace this guest does not
//!     own, via the ordinary public SDK client (no WRAP meta — the SDK
//!     doesn't know WRAP exists).
//!   - `"test.query_raw_secrets"` — same shape, `query_raw` reading a
//!     foreign collection.
//!
//! This is not a "bypass" — it's what an ordinary, unprivileged, honestly
//! written wasm block looks like when built against the public Rust SDK.
//! That is precisely the meta-omission vector SP-A closes: enforcement must
//! not depend on the guest having set anything.

use wafer_sdk::{
    clients::database,
    core_abi::{pack_ptr_len, GuestResult},
    wire::database::{ExecRawRequest, QueryRawRequest},
    BlockInfo, ErrorCode, Message, WaferError,
};

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
    );
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
                query: "SELECT * FROM suppers_ai__other_block__secrets".to_string(),
                args: vec![],
            };
            match database::query_raw(&req) {
                Ok(records) => GuestResult::respond(
                    serde_json::to_vec(&records).expect("Vec<Record> is JSON-serialisable"),
                ),
                Err(e) => GuestResult::error(e),
            }
        }
        other => GuestResult::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("hostile-db-guest: unknown kind {other}"),
        )),
    }
}
