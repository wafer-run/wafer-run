//! Warm-instance-pooling test guest (PERF-01 Part B — see
//! `tests/wasm_instance_pooling.rs`).
//!
//! Implements the standard `__wafer_handle(ptr, len) -> i64` ABI v2 glue
//! directly (same shape as `benches/fixtures/bench_guest`) so the fixture
//! stays self-contained and explicit. The one piece of state is a global
//! call counter living in this instance's linear memory: its value across
//! calls IS the observable "was this instance reused?" signal every test
//! keys off.
//!
//! Two build variants from this one crate (see `scripts/build-fixtures.sh`):
//! the default declares `InstanceMode::Singleton` (pool-eligible), the
//! `percall` feature declares `InstanceMode::PerExecution` (must stay cold).
//!
//! Operations, selected by `msg.kind` (every arm increments the counter
//! first, so the response — or the state the host observes afterwards —
//! reflects how many calls this *instance* has served):
//!
//!   - `"pool.count"` — respond with the counter as decimal ASCII.
//!     Pooled: sequential calls see `1`, `2`, …; cold: always `1`.
//!   - `"pool.trap"` — hit `unreachable` (a wasm trap). The host must drop
//!     the instance; the next call starts a fresh one (counter back to `1`).
//!   - `"pool.leak_stream"` — open a raw stream handle via
//!     `__wafer_host_stream_init` and never close it, then respond with
//!     `"{counter}:{handle}"`. Handles are allocated per-registry starting
//!     at 1, so a reused instance whose registry was properly drained
//!     reports handle `1` again; an undrained registry would report `2`.
//!   - `"pool.grow_to_cap"` — `memory.grow` one page at a time until the
//!     host's `ResourceLimiter` denies growth (linear memory is now exactly
//!     at the block's page cap), then respond with the counter. The host
//!     must recycle the instance on checkin.
//!   - `"pool.gate"` — nested streaming `call_block` to `test/pool-gate`
//!     (a native block the concurrency tests park on a barrier), then
//!     respond with the counter. Overlapping calls prove distinct instances
//!     by each reporting counter `1`.

use core::sync::atomic::{AtomicU32, Ordering};

use wafer_sdk::core_abi::{pack_ptr_len, GuestResult};
use wafer_sdk::stream::CallStream;
use wafer_sdk::{BlockInfo, ErrorCode, InstanceMode, Message, WaferError};

/// Per-instance call counter. Lives in linear memory, so it survives across
/// calls exactly when the host reuses the instance.
static CALLS: AtomicU32 = AtomicU32::new(0);

// Raw stream-open import. The SDK's `CallStream` closes its handle on `Drop`
// (leak-proof by design), so a fixture that must *leak* a live handle — to
// prove the host drains the registry on checkin — has to call the host
// import directly.
#[link(wasm_import_module = "wafer")]
extern "C" {
    fn __wafer_host_stream_init(name_ptr: i32, name_len: i32, msg_ptr: i32, msg_len: i32) -> i64;
}

/// Block metadata export. Returns a codec-encoded `BlockInfo` packed as
/// `(ptr << 32) | len` — the format `WasmiBlock::info()` expects.
#[no_mangle]
pub extern "C" fn __wafer_info() -> i64 {
    #[cfg(feature = "percall")]
    let mode = InstanceMode::PerExecution;
    #[cfg(not(feature = "percall"))]
    let mode = InstanceMode::Singleton;

    let info = BlockInfo::new(
        "test/pool-guest",
        "0.0.0",
        "handler@v1",
        "Warm-instance-pooling test guest — global counter + trap/leak/grow arms",
    )
    .instance_mode(mode)
    // SEC-02: WASM guests fail closed by default, so declare the native
    // gate block the `pool.gate` / `pool.leak_stream` arms target.
    .capabilities(wafer_sdk::BlockCapabilities {
        callable_blocks: ["test/pool-gate"].into_iter().map(String::from).collect(),
        ..wafer_sdk::BlockCapabilities::none()
    });
    let bytes = wafer_sdk::codec::encode(&info).expect("BlockInfo is codec-serialisable");
    let ptr = bytes.as_ptr() as u32;
    let len = bytes.len() as u32;
    std::mem::forget(bytes);
    pack_ptr_len(ptr, len)
}

/// Core-ABI version export (v2 = MessagePack frames).
#[no_mangle]
pub extern "C" fn __wafer_abi_version() -> i32 {
    wafer_sdk::abi::ABI_VERSION
}

/// Lifecycle hook — no-op. Deliberately does NOT touch the counter: the
/// host runs lifecycle events on fresh (never pooled) instances, so a
/// counter bump here would be invisible anyway and only muddy the tests.
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
        Ok(frame) => dispatch(&frame.0),
        Err(e) => GuestResult::error(WaferError::new(
            ErrorCode::InvalidArgument,
            format!("pool-guest: invalid call frame: {e}"),
        )),
    };
    let result_bytes =
        wafer_sdk::codec::encode(&result).expect("GuestResult is always codec-serialisable");
    let ptr = result_bytes.as_ptr() as u32;
    let len = result_bytes.len() as u32;
    std::mem::forget(result_bytes);
    pack_ptr_len(ptr, len)
}

fn dispatch(msg: &Message) -> GuestResult {
    let calls = CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    match msg.kind.as_str() {
        "pool.count" => GuestResult::respond(calls.to_string().into_bytes()),
        "pool.trap" => core::arch::wasm32::unreachable(),
        "pool.leak_stream" => leak_stream(calls),
        "pool.grow_to_cap" => {
            // Grow one page at a time until the host's ResourceLimiter says
            // no — linear memory is then exactly at the block's page cap.
            while core::arch::wasm32::memory_grow(0, 1) != usize::MAX {}
            GuestResult::respond(calls.to_string().into_bytes())
        }
        "pool.gate" => gate_roundtrip(calls),
        other => GuestResult::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("pool-guest: unknown kind {other}"),
        )),
    }
}

/// Open a stream handle and deliberately never close it. Responds with
/// `"{counter}:{handle}"` so the test can assert both instance reuse (the
/// counter) and registry drain (the handle numbering restarting at 1).
fn leak_stream(calls: u32) -> GuestResult {
    let msg = Message::new("pool.leaked");
    let msg_bytes = match wafer_sdk::codec::encode(&msg) {
        Ok(b) => b,
        Err(e) => {
            return GuestResult::error(WaferError::new(
                ErrorCode::Internal,
                format!("pool-guest: encoding leak message: {e}"),
            ));
        }
    };
    let target = "test/pool-gate";
    let handle = unsafe {
        __wafer_host_stream_init(
            target.as_ptr() as i32,
            target.len() as i32,
            msg_bytes.as_ptr() as i32,
            msg_bytes.len() as i32,
        )
    };
    if handle < 0 {
        return GuestResult::error(WaferError::new(
            ErrorCode::Internal,
            format!("pool-guest: stream_init failed with sentinel {handle}"),
        ));
    }
    GuestResult::respond(format!("{calls}:{handle}").into_bytes())
}

/// Nested streaming call to the native gate block: write one chunk, finish,
/// drain the response, respond with the counter.
fn gate_roundtrip(calls: u32) -> GuestResult {
    let msg = Message::new("gate.wait");
    let mut stream = match CallStream::open("test/pool-gate", &msg) {
        Ok(s) => s,
        Err(e) => return GuestResult::error(e),
    };
    if let Err(e) = stream.write_chunk(b"x") {
        return GuestResult::error(e);
    }
    let mut response = match stream.finish() {
        Ok(r) => r,
        Err(e) => return GuestResult::error(e),
    };
    loop {
        match response.next_chunk() {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(e) => return GuestResult::error(e),
        }
    }
    GuestResult::respond(calls.to_string().into_bytes())
}
