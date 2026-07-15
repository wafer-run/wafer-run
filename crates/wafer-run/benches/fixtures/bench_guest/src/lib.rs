//! Benchmark guest for the criterion suite in `crates/wafer-run/benches/`
//! (PERF-01 measurement prerequisite).
//!
//! Implements the standard `__wafer_handle(ptr, len) -> i64` ABI directly
//! (same shape as `tests/dispatch_guest`) so the fixture stays self-contained
//! and explicit. Operations, selected by `msg.kind`:
//!
//!   - `"bench.echo"` — returns the request body unchanged. Used by the
//!     ABI round-trip benchmarks (1 KiB / 64 KiB / 1 MiB payloads): the
//!     measured cost is per-call instantiation + the core-ABI v2
//!     (MessagePack, bin-encoded bodies) framing on both sides, not guest work.
//!   - `"bench.nested_native"` — opens a streaming call to the native
//!     `bench/native-echo` block, writes the request body as one chunk,
//!     drains the response, and returns the concatenated bytes. Measures a
//!     guest→native nested `call_block`.
//!   - `"bench.nested_wasm"` — same, but the callee is this guest itself
//!     (registered as `bench/guest`, invoked with `bench.echo`). Measures a
//!     guest→guest nested call, which pays a second per-call instantiation.

use wafer_sdk::core_abi::{pack_ptr_len, GuestResult};
use wafer_sdk::stream::CallStream;
use wafer_sdk::{BlockInfo, ErrorCode, Message, WaferError};

/// Block metadata export. Returns a codec-encoded `BlockInfo` packed as
/// `(ptr << 32) | len` — the format `WasmiBlock::info()` expects.
#[no_mangle]
pub extern "C" fn __wafer_info() -> i64 {
    let info = BlockInfo::new(
        "bench/guest",
        "0.0.0",
        "handler@v1",
        "Criterion benchmark guest — echo + nested call_block arms",
    )
    // SEC-02: WASM guests fail closed by default, so this fixture must
    // declare the blocks it calls — the native echo block and itself
    // (for the guest→guest nested arm).
    .capabilities(wafer_sdk::BlockCapabilities {
        callable_blocks: ["bench/native-echo", "bench/guest"]
            .into_iter()
            .map(String::from)
            .collect(),
        ..wafer_sdk::BlockCapabilities::none()
    });
    let bytes = wafer_sdk::codec::encode(&info).expect("BlockInfo is codec-serialisable");
    let ptr = bytes.as_ptr() as u32;
    let len = bytes.len() as u32;
    std::mem::forget(bytes);
    pack_ptr_len(ptr, len)
}

/// Core-ABI version export (v2 = MessagePack frames). The benches measure
/// the framing the ecosystem actually ships, which is the current SDK's.
#[no_mangle]
pub extern "C" fn __wafer_abi_version() -> i32 {
    wafer_sdk::abi::ABI_VERSION
}

/// Lifecycle hook — no-op for this fixture. Returns `Result<(), WaferError>::Ok(())`.
#[no_mangle]
pub extern "C" fn __wafer_lifecycle(_evt_ptr: i32, _evt_len: i32) -> i64 {
    let bytes = wafer_sdk::codec::encode(&Ok::<(), WaferError>(()))
        .expect("Result<(), WaferError>::Ok(()) is codec-serialisable");
    let ptr = bytes.as_ptr() as u32;
    let len = bytes.len() as u32;
    std::mem::forget(bytes);
    pack_ptr_len(ptr, len)
}

/// Standard wafer block handler entry point (ABI v2: MessagePack frames).
#[no_mangle]
pub extern "C" fn __wafer_handle(msg_ptr: i32, msg_len: i32) -> i64 {
    let msg_bytes = unsafe { std::slice::from_raw_parts(msg_ptr as *const u8, msg_len as usize) };
    let result = match wafer_sdk::codec::decode::<wafer_sdk::abi::CallFrame>(msg_bytes) {
        Ok(frame) => dispatch(&frame.0, frame.1.into_vec()),
        Err(e) => GuestResult::error(WaferError::new(
            ErrorCode::InvalidArgument,
            format!("bench-guest: invalid call frame: {e}"),
        )),
    };
    let result_bytes =
        wafer_sdk::codec::encode(&result).expect("GuestResult is always codec-serialisable");
    let ptr = result_bytes.as_ptr() as u32;
    let len = result_bytes.len() as u32;
    std::mem::forget(result_bytes);
    pack_ptr_len(ptr, len)
}

fn dispatch(msg: &Message, body: Vec<u8>) -> GuestResult {
    match msg.kind.as_str() {
        "bench.echo" => GuestResult::respond(body),
        "bench.nested_native" => nested_echo("bench/native-echo", &body),
        "bench.nested_wasm" => nested_echo("bench/guest", &body),
        other => GuestResult::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("bench-guest: unknown kind {other}"),
        )),
    }
}

/// Open a streaming call to `target` under `bench.echo`, send `body` as one
/// chunk, drain the response, and respond with the concatenated bytes.
fn nested_echo(target: &str, body: &[u8]) -> GuestResult {
    let inner_msg = Message::new("bench.echo");

    let mut stream = match CallStream::open(target, &inner_msg) {
        Ok(s) => s,
        Err(e) => return GuestResult::error(e),
    };
    if let Err(e) = stream.write_chunk(body) {
        return GuestResult::error(e);
    }
    let mut response = match stream.finish() {
        Ok(r) => r,
        Err(e) => return GuestResult::error(e),
    };

    let mut full = Vec::new();
    loop {
        match response.next_chunk() {
            Ok(Some(chunk)) => full.extend_from_slice(&chunk),
            Ok(None) => break,
            Err(e) => return GuestResult::error(e),
        }
    }
    GuestResult::respond(full)
}
