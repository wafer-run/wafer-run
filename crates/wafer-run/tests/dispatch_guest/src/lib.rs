//! End-to-end dispatch test guest for the binary-transport overhaul.
//!
//! Implements the standard `__wafer_handle(ptr, len) -> i64` ABI directly
//! (without the `#[wafer_block]` macro) so the test fixture stays
//! self-contained and explicit. Two operations, selected by `msg.kind`:
//!
//!   - `"test.buffered"`  — calls `wafer_sdk::clients::network::do_request`
//!     and returns the buffered body bytes.
//!   - `"test.streaming"` — calls `wafer_sdk::clients::network::do_request_stream`,
//!     drains the body via `next_chunk()`, and returns the concatenated
//!     bytes as the response body.
//!
//! The request body sent to this guest is the URL to fetch, as plain UTF-8
//! bytes (e.g. `b"https://example.test/buffered"`). This keeps input
//! decoding trivial and unrelated to what the test is actually proving:
//! that the SDK clients correctly drive the new streaming/codec ABI.

use std::collections::HashMap;

use wafer_sdk::{
    clients::network,
    core_abi::{pack_ptr_len, GuestResult},
    stream::CallStream,
    BlockInfo, ErrorCode, Message, MetaEntry, WaferError,
};

// `__wafer_alloc` is exported by `wafer_sdk::core_abi` on `wasm32-*` targets,
// so we don't redefine it here.

/// Block metadata export. Returns a JSON-encoded `BlockInfo` packed as
/// `(ptr << 32) | len` — the format `WasmiBlock::info()` expects.
#[no_mangle]
pub extern "C" fn __wafer_info() -> i64 {
    let info = BlockInfo::new(
        "test/dispatch-guest",
        "0.0.0",
        "handler@v1",
        "E2E dispatch test guest — exercises wafer-sdk typed network client",
    )
    // SEC-02: declare the network service block this guest calls and the
    // network capability its requests exercise (checked against the
    // caller's caps in `check_resource_access`).
    .capabilities(wafer_sdk::BlockCapabilities {
        // `test/recorder` is the SEC-01 nested-call e2e callee (a native
        // meta-echo block registered by dispatch_streaming.rs).
        callable_blocks: ["wafer-run/network", "test/recorder"]
            .into_iter()
            .map(String::from)
            .collect(),
        network: true,
        ..wafer_sdk::BlockCapabilities::none()
    });
    let bytes = serde_json::to_vec(&info).expect("BlockInfo is JSON-serialisable");
    let ptr = bytes.as_ptr() as u32;
    let len = bytes.len() as u32;
    std::mem::forget(bytes);
    pack_ptr_len(ptr, len)
}

/// Lifecycle hook — no-op for this test fixture. Returns an empty
/// JSON `Ok` result. The host calls this on `Init` (and other lifecycle
/// events); the value of `evt_*` is ignored.
#[no_mangle]
pub extern "C" fn __wafer_lifecycle(_evt_ptr: i32, _evt_len: i32) -> i64 {
    // Result<(), WaferError>::Ok(()) — serializes as `{"Ok": null}` per serde.
    let bytes = serde_json::to_vec(&Ok::<(), WaferError>(()))
        .expect("Result<(), WaferError>::Ok(()) is JSON-serialisable");
    let ptr = bytes.as_ptr() as u32;
    let len = bytes.len() as u32;
    std::mem::forget(bytes);
    pack_ptr_len(ptr, len)
}

/// Standard wafer block handler entry point.
///
/// Decodes the host-supplied `(Message, Vec<u8>)` JSON tuple, dispatches
/// on `msg.kind`, and returns a JSON-encoded `GuestResult`.
#[no_mangle]
pub extern "C" fn __wafer_handle(msg_ptr: i32, msg_len: i32) -> i64 {
    let msg_bytes = unsafe { std::slice::from_raw_parts(msg_ptr as *const u8, msg_len as usize) };
    let result = match serde_json::from_slice::<(Message, Vec<u8>)>(msg_bytes) {
        Ok((msg, body)) => dispatch(&msg, &body),
        Err(e) => GuestResult::error(WaferError::new(
            ErrorCode::InvalidArgument,
            format!("dispatch-guest: invalid (Message, body) tuple: {e}"),
        )),
    };
    let result_bytes =
        serde_json::to_vec(&result).expect("GuestResult is always JSON-serialisable");
    let ptr = result_bytes.as_ptr() as u32;
    let len = result_bytes.len() as u32;
    std::mem::forget(result_bytes);
    pack_ptr_len(ptr, len)
}

fn dispatch(msg: &Message, body: &[u8]) -> GuestResult {
    let url = match std::str::from_utf8(body) {
        Ok(u) => u,
        Err(_) => {
            return GuestResult::error(WaferError::new(
                ErrorCode::InvalidArgument,
                "dispatch-guest: body is not valid UTF-8",
            ))
        }
    };
    let headers: HashMap<String, String> = HashMap::new();

    match msg.kind.as_str() {
        "test.buffered" => match network::do_request("GET", url, &headers, None) {
            Ok(resp) => GuestResult::respond(resp.body),
            Err(e) => GuestResult::error(e),
        },
        "test.streaming" => match network::do_request_stream("GET", url, &headers, None) {
            Ok(mut stream) => {
                let mut full = Vec::new();
                loop {
                    match stream.next_chunk() {
                        Ok(Some(chunk)) => full.extend_from_slice(&chunk),
                        Ok(None) => break,
                        Err(e) => return GuestResult::error(e),
                    }
                }
                GuestResult::respond(full)
            }
            Err(e) => GuestResult::error(e),
        },
        // SEC-01 hostile arm: attempt to forge host-owned identity metadata in
        // the response. The host must strip `auth.*` on egress; only the
        // non-protected `trace_id` should survive.
        "test.forge_respond" => GuestResult::respond_with_meta(
            b"forged".to_vec(),
            vec![
                MetaEntry {
                    key: "auth.user_roles".to_string(),
                    value: "admin".to_string(),
                },
                MetaEntry {
                    key: "auth.user_id".to_string(),
                    value: "attacker".to_string(),
                },
                MetaEntry {
                    key: "trace_id".to_string(),
                    value: "t1".to_string(),
                },
            ],
        ),
        // SEC-01 hostile arm: forge host-owned identity on the Continue path.
        // The continuation message the guest hands to the next flow step
        // claims to be an admin; the host must drop the forged `auth.*` keys
        // and restore the inbound host-set values, preserving `trace_id`.
        "test.forge_continue" => {
            let mut next = Message::new("next.step");
            next.set_meta("auth.user_id", "attacker");
            next.set_meta("auth.user_roles", "admin");
            next.set_meta("trace_id", "t1");
            GuestResult {
                action: "Continue".to_string(),
                response: None,
                error: None,
                message: Some(next),
            }
        }
        // SEC-01 hostile arm: forge host-owned identity on the nested
        // `call_block` (streaming host ABI) path. The callee `test/recorder`
        // echoes the meta it receives as its JSON body, so the outer test can
        // assert exactly what identity crossed the boundary.
        "test.forge_nested_call" => {
            let mut nested = Message::new("record.echo");
            nested.set_meta("auth.user_id", "attacker");
            nested.set_meta("auth.user_roles", "admin");
            nested.set_meta("trace_id", "t2");
            let response = CallStream::open("test/recorder", &nested)
                .and_then(|call| call.finish())
                .and_then(|mut resp| Ok(resp.next_chunk()?.unwrap_or_default()));
            match response {
                Ok(body) => GuestResult::respond(body),
                Err(e) => GuestResult::error(e),
            }
        }
        other => GuestResult::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("dispatch-guest: unknown kind {other}"),
        )),
    }
}
