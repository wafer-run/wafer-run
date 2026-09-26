//! End-to-end fixture for TODO #103: a WASM guest that calls a **wafer-core**
//! typed service client.
//!
//! Implements the standard `__wafer_handle(ptr, len) -> i64` ABI directly
//! (without the `#[wafer_block]` macro) so the fixture stays self-contained.
//! Operations, selected by `msg.kind`:
//!
//!   - `"test.config_get"` — reads a config key (the request body, as plain
//!     UTF-8) via `wafer_core::clients::config::get`, and returns the resolved
//!     value as the response body.
//!   - `"test.db_create_many"` / `"test.db_batch"` — insert N rows (the
//!     request body, a decimal count) into the guest's own collection
//!     [`OWN_COLLECTION`] via `wafer_core::clients::database::create_many` /
//!     `::batch`, and return the host's error unchanged as the guest's own.
//!
//! Unlike `dispatch_guest` (which exercises the *SDK's* clients), this guest
//! calls `wafer_core::clients::*`, driving wafer-core's wasm-component
//! `call_service` → streaming-ABI host imports. It is the first fixture to
//! depend on `wafer-core` compiled with `--features wasm-component`.

use std::collections::HashMap;

use wafer_core::clients::{config, database};
use wafer_sdk::core_abi::{pack_ptr_len, GuestResult};
use wafer_sdk::wire::database::BatchWrite;
use wafer_sdk::{BlockInfo, ErrorCode, Message, WaferError};

/// Collection the guest owns under the WRAP namespace convention
/// (`{org}__{block}__{name}` → `test/service-client-guest`).
const OWN_COLLECTION: &str = "test__service_client_guest__items";

// `__wafer_alloc` is exported by `wafer_sdk::core_abi` on `wasm32-*` targets,
// so we don't redefine it here.

/// Block metadata export. Returns a JSON-encoded `BlockInfo` packed as
/// `(ptr << 32) | len` — the format `WasmiBlock::info()` expects.
#[no_mangle]
pub extern "C" fn __wafer_info() -> i64 {
    let info = BlockInfo::new(
        "test/service-client-guest",
        "0.0.0",
        "handler@v1",
        "E2E fixture — exercises wafer-core's wasm-component typed config client",
    )
    // SEC-02: declare the config service block this guest calls and the
    // config capability its reads exercise.
    .capabilities(wafer_sdk::BlockCapabilities {
        callable_blocks: wafer_sdk::Allowlist::Only(["wafer-run/config", "wafer-run/database"] .into_iter() .map(String::from) .collect()),
        config: wafer_sdk::Allowlist::Any,
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
            format!("service-client-guest: invalid (Message, body) tuple: {e}"),
        )),
    };
    let result_bytes = serde_json::to_vec(&result).expect("GuestResult is always JSON-serialisable");
    let ptr = result_bytes.as_ptr() as u32;
    let len = result_bytes.len() as u32;
    std::mem::forget(result_bytes);
    pack_ptr_len(ptr, len)
}

fn dispatch(msg: &Message, body: &[u8]) -> GuestResult {
    match msg.kind.as_str() {
        "test.config_get" => {
            let key = match std::str::from_utf8(body) {
                Ok(k) => k,
                Err(_) => {
                    return GuestResult::error(WaferError::new(
                        ErrorCode::InvalidArgument,
                        "service-client-guest: config key body is not valid UTF-8",
                    ))
                }
            };
            // The call under test: wafer-core's wasm-component config client,
            // which drives `call_service` over the streaming ABI to the host.
            match config::get(key) {
                Ok(value) => GuestResult::respond(value.into_bytes()),
                Err(e) => GuestResult::error(e),
            }
        }
        "test.db_create_many" | "test.db_batch" => {
            let rows = match row_count(body) {
                Ok(n) => (0..n).map(|i| {
                    HashMap::from([("name".to_string(), serde_json::json!(format!("r{i}")))])
                }),
                Err(e) => return GuestResult::error(e),
            };
            let outcome = if msg.kind == "test.db_create_many" {
                database::create_many(OWN_COLLECTION, rows.collect()).map(|n| n.to_string())
            } else {
                let ops = rows
                    .map(|data| BatchWrite::Create {
                        collection: OWN_COLLECTION.to_string(),
                        data,
                    })
                    .collect();
                database::batch(ops).map(|results| results.len().to_string())
            };
            match outcome {
                Ok(n) => GuestResult::respond(n.into_bytes()),
                Err(e) => GuestResult::error(e),
            }
        }
        other => GuestResult::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("service-client-guest: unknown kind {other}"),
        )),
    }
}

/// The row count a `test.db_*` request body carries, as decimal text.
fn row_count(body: &[u8]) -> Result<usize, WaferError> {
    std::str::from_utf8(body)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| {
            WaferError::new(
                ErrorCode::InvalidArgument,
                "service-client-guest: the row count body is not a decimal number",
            )
        })
}
