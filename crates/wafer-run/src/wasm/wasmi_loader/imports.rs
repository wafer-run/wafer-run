//! Host-side wasmi imports: [`build_linker`] registers every `func_wrap`
//! the guest can call (the `wafer` ABI, streaming call ABI, and WASI shims).
//! Split out of the loader so the registration code lives apart from the
//! [`super::WasmiBlock`] runtime.

use tracing::warn;
use wafer_block::{core_types::*, error::RuntimeError};
use wasmi::{Caller, Engine, Error as WasmiError, Linker};

use super::abi::*;
use crate::wasm::stream::StreamState;

// ---------------------------------------------------------------------------
// WASI errno constants (wasi_snapshot_preview1)
// ---------------------------------------------------------------------------

/// `__WASI_ERRNO_SUCCESS` — the WASI call completed without error.
const WASI_ERRNO_SUCCESS: i32 = 0;

/// `__WASI_ERRNO_FAULT` — a bad address was passed to the WASI call (e.g. a
/// guest pointer that does not map into the exported linear memory). Returned
/// from the WASI shims when a `memory.read`/`memory.write` fails so the guest
/// sees a real failure instead of "success" plus uninitialised memory.
const WASI_ERRNO_FAULT: i32 = 21;

/// `__WASI_ERRNO_IO` — an I/O error occurred. Returned from `random_get` when
/// the host RNG fails, so the guest never observes a "success" return paired
/// with a non-random (zero-filled) buffer.
const WASI_ERRNO_IO: i32 = 29;

// ---------------------------------------------------------------------------
// Linker setup
// ---------------------------------------------------------------------------

pub(super) fn build_linker(engine: &Engine) -> Result<Linker<WasmiHostState>, RuntimeError> {
    let mut linker = Linker::<WasmiHostState>::new(engine);

    // ---- wafer module: host imports ----

    // __wafer_host_is_cancelled() -> i32
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_is_cancelled",
            |caller: Caller<WasmiHostState>| -> i32 {
                let state = caller.data();
                state
                    .context
                    .as_ref()
                    .map_or(0, |ctx| i32::from(ctx.is_cancelled()))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_is_cancelled: {e}")))?;

    // __wafer_host_log(level_ptr, level_len, msg_ptr, msg_len)
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_log",
            |caller: Caller<WasmiHostState>,
             level_ptr: i32,
             level_len: i32,
             msg_ptr: i32,
             msg_len: i32| {
                let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return;
                };
                let Ok(level_bytes) = read_guest_slice(&caller, memory, level_ptr, level_len)
                else {
                    return;
                };
                let Ok(msg_bytes) = read_guest_slice(&caller, memory, msg_ptr, msg_len) else {
                    return;
                };
                let level = String::from_utf8_lossy(&level_bytes);
                let msg = String::from_utf8_lossy(&msg_bytes);
                match level.as_ref() {
                    "error" => tracing::error!(target: "wasm_guest", "{msg}"),
                    "warn" => tracing::warn!(target: "wasm_guest", "{msg}"),
                    "debug" => tracing::debug!(target: "wasm_guest", "{msg}"),
                    "trace" => tracing::trace!(target: "wasm_guest", "{msg}"),
                    _ => tracing::info!(target: "wasm_guest", "{msg}"),
                }
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_log: {e}")))?;

    // ---- Streaming call ABI: 6 host imports ------------------------------
    //
    // Lifecycle: stream_init → write_chunk* → finish → read_chunk* → close.
    // See `wasm/stream.rs` for the StreamState state machine.
    //
    // Synchronous host imports (no trap): init, write_chunk, close.
    // Trap+resume host imports (need async dispatch or guest allocation):
    // finish, read_chunk, take_error.
    //
    // Performance envelope (measured by the pre-implementation spike harness,
    // removed once this ABI landed): per-resumable-call dispatch overhead
    // < 100 µs/chunk at 1 KiB chunks; sustained host→guest throughput
    // ≥ 100 MiB/s at 512 KiB chunks (empirical ceiling ~140 MiB/s —
    // `memory.write` into linear memory dominates at large chunk sizes).

    // __wafer_host_stream_init(name_ptr, name_len, msg_ptr, msg_len) -> i64
    //
    // Reads the target block name + serialized Message from guest memory,
    // allocates a fresh StreamState in the registry, returns the handle as
    // a positive i64. On capability denial returns a negative i64 carrying
    // the ErrorCode sentinel (the guest can call `take_error` for details
    // — but `init` failure leaves no handle, so the SDK must treat negative
    // returns as immediate errors without further state.)
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_init",
            |mut caller: Caller<WasmiHostState>,
             name_ptr: i32,
             name_len: i32,
             msg_ptr: i32,
             msg_len: i32|
             -> Result<i64, WasmiError> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| WasmiError::new("guest has no exported memory"))?;

                let name_buf = read_guest_slice(&caller, memory, name_ptr, name_len)
                    .map_err(|e| WasmiError::new(format!("reading block name: {e}")))?;
                let block_name = String::from_utf8(name_buf)
                    .map_err(|e| WasmiError::new(format!("block name not UTF-8: {e}")))?;

                // Capability check: deny if block is not allowed to call target.
                if !caller.data().capabilities.allows_call_block(&block_name) {
                    return Ok(error_code_to_neg_i64(ErrorCode::PermissionDenied));
                }

                let msg_buf = read_guest_slice(&caller, memory, msg_ptr, msg_len)
                    .map_err(|e| WasmiError::new(format!("reading stream message: {e}")))?;

                // Decode the message. The SDK encodes via rmp-serde (codec
                // module); historically the guest sent JSON. We try rmp first
                // and fall back to JSON for forward compatibility with any
                // pre-codec callers — but the production path is rmp.
                let msg: Message = match wafer_block::codec::decode::<Message>(&msg_buf) {
                    Ok(m) => m,
                    Err(_) => match serde_json::from_slice::<Message>(&msg_buf) {
                        Ok(m) => m,
                        Err(_) => {
                            return Ok(error_code_to_neg_i64(ErrorCode::InvalidArgument));
                        }
                    },
                };

                let state = StreamState::new(block_name, msg);
                let handle = caller.data_mut().streams.alloc(state);
                Ok(handle as i64)
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_init: {e}")))?;

    // __wafer_host_stream_write_chunk(handle, body_ptr, body_len) -> i32
    //
    // Append a chunk to the request buffer for the given handle. Returns 0
    // on success, negative ErrorCode sentinel on error.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_write_chunk",
            |caller: Caller<WasmiHostState>,
             handle: i64,
             body_ptr: i32,
             body_len: i32|
             -> Result<i32, WasmiError> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| WasmiError::new("guest has no exported memory"))?;

                let buf = read_guest_slice(&caller, memory, body_ptr, body_len)
                    .map_err(|e| WasmiError::new(format!("reading stream chunk: {e}")))?;

                let mut caller = caller;
                let Some(state) = caller.data_mut().streams.get_mut(handle as u64) else {
                    return Ok(error_code_to_neg_i32(ErrorCode::NotFound));
                };
                match state.write_chunk(&buf) {
                    Ok(()) => Ok(0),
                    Err(e) => {
                        let code = e.code;
                        state.record_error_and_close(e);
                        Ok(error_code_to_neg_i32(code))
                    }
                }
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_write_chunk: {e}")))?;

    // __wafer_host_stream_attach(handle, payload_ptr, payload_len) -> i32
    //
    // Decodes a rmp-encoded (id: String, attachment: Attachment) tuple from
    // guest memory and adds it to the caller StreamState's attachments map.
    // Returns 0 on success, negative ErrorCode sentinel on error:
    //   - NotFound: stream handle invalid
    //   - FailedPrecondition: stream not in WritingRequest phase
    //   - InvalidArgument: payload undecodable
    //   - Internal: unrecoverable host-side error
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_attach",
            |mut caller: Caller<WasmiHostState>,
             handle: i64,
             payload_ptr: i32,
             payload_len: i32|
             -> Result<i32, WasmiError> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| WasmiError::new("guest has no exported memory".to_string()))?;
                let buf = read_guest_slice(&caller, memory, payload_ptr, payload_len)
                    .map_err(|e| WasmiError::new(format!("reading attach payload: {e}")))?;

                let (id, att): (String, wafer_block::Attachment) =
                    match wafer_block::codec::decode(&buf) {
                        Ok(v) => v,
                        Err(_) => return Ok(error_code_to_neg_i32(ErrorCode::InvalidArgument)),
                    };

                let Some(stream_state) = caller.data_mut().streams.get_mut(handle as u64) else {
                    return Ok(error_code_to_neg_i32(ErrorCode::NotFound));
                };

                match stream_state.attach(id, att) {
                    Ok(()) => Ok(0),
                    Err(_) => Ok(error_code_to_neg_i32(ErrorCode::FailedPrecondition)),
                }
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_attach: {e}")))?;

    // __wafer_host_stream_finish(handle) -> i32
    //
    // Traps. The resume loop dispatches `Context::call_block(target, msg,
    // InputStream::from_bytes(buf))`, installs the OutputStream on the
    // StreamState, and resumes with 0. On dispatch error the loop records
    // the error on the state and resumes with a negative ErrorCode sentinel.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_finish",
            |mut caller: Caller<WasmiHostState>, handle: i64| -> Result<i32, WasmiError> {
                if caller.data_mut().streams.get_mut(handle as u64).is_none() {
                    return Ok(error_code_to_neg_i32(ErrorCode::NotFound));
                }
                caller.data_mut().pending_stream_finish = Some(handle as u64);
                Err(WasmiError::host(StreamFinishTrap))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_finish: {e}")))?;

    // __wafer_host_stream_read_chunk(handle) -> i64
    //
    // Traps. The resume loop drives the OutputStream's next frame; on a
    // Chunk it allocates guest memory + writes the bytes and resumes with
    // the packed (ptr, len). On Complete it resumes with 0 (end-of-stream).
    // On Error/Drop/Continue it records the error on the state and resumes
    // with a negative ErrorCode sentinel.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_read_chunk",
            |mut caller: Caller<WasmiHostState>, handle: i64| -> Result<i64, WasmiError> {
                if caller.data_mut().streams.get_mut(handle as u64).is_none() {
                    return Ok(error_code_to_neg_i64(ErrorCode::NotFound));
                }
                caller.data_mut().pending_stream_read = Some(handle as u64);
                Err(WasmiError::host(StreamReadTrap))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_read_chunk: {e}")))?;

    // __wafer_host_stream_take_error(handle) -> i64
    //
    // Traps. The resume loop pops `last_error` off the StreamState, encodes
    // via rmp-serde, allocates guest memory + writes, resumes with packed
    // (ptr, len). If no error is present, resumes with 0.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_take_error",
            |mut caller: Caller<WasmiHostState>, handle: i64| -> Result<i64, WasmiError> {
                if caller.data_mut().streams.get_mut(handle as u64).is_none() {
                    return Ok(error_code_to_neg_i64(ErrorCode::NotFound));
                }
                caller.data_mut().pending_stream_take_error = Some(handle as u64);
                Err(WasmiError::host(StreamTakeErrorTrap))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_take_error: {e}")))?;

    // __wafer_host_stream_close(handle)
    //
    // Synchronous: drop the stream. Idempotent — no-op if the handle is
    // unknown. Cancels any in-flight response stream via the OutputStream
    // drop on its CancellationToken.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_close",
            |mut caller: Caller<WasmiHostState>, handle: i64| {
                caller.data_mut().streams.close(handle as u64);
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_close: {e}")))?;

    // __wafer_host_lookup_attachment(id_ptr, id_len) -> i64
    //
    // Returns:
    //   - Negative ErrorCode sentinel (NotFound) if the current call frame has
    //     no attachment under id.
    //   - Negative ErrorCode sentinel (InvalidArgument) if id is not valid UTF-8.
    //   - Negative ErrorCode sentinel (Internal) if encoding fails or guest-memory
    //     allocation/write fails.
    //   - Otherwise, positive packed (ptr, len) of an rmp-encoded Attachment,
    //     written via the guest's __wafer_alloc export. Guest owns the allocation.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_lookup_attachment",
            |mut caller: Caller<WasmiHostState>,
             id_ptr: i32,
             id_len: i32|
             -> Result<i64, WasmiError> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| WasmiError::new("guest has no exported memory".to_string()))?;

                let id_buf = read_guest_slice(&caller, memory, id_ptr, id_len)
                    .map_err(|e| WasmiError::new(format!("reading attachment id: {e}")))?;

                let id = match std::str::from_utf8(&id_buf) {
                    Ok(s) => s.to_string(),
                    Err(_) => return Ok(error_code_to_neg_i64(ErrorCode::InvalidArgument)),
                };

                // Clone the attachment before any mutable borrow of caller.
                let att = match caller
                    .data()
                    .current_attachments
                    .as_ref()
                    .and_then(|m| m.get(&id))
                {
                    Some(a) => a.clone(),
                    None => return Ok(error_code_to_neg_i64(ErrorCode::NotFound)),
                };

                // Encode the Attachment via rmp.
                let Ok(encoded) = wafer_block::codec::encode(&att) else {
                    return Ok(error_code_to_neg_i64(ErrorCode::Internal));
                };

                // Allocate guest memory via __wafer_alloc (same pattern used in
                // write_guest_bytes), then write the encoded bytes. The guest
                // owns the allocation after this call returns.
                let alloc_func = caller
                    .get_export("__wafer_alloc")
                    .and_then(|e| e.into_func())
                    .ok_or_else(|| {
                        WasmiError::new("guest has no __wafer_alloc export".to_string())
                    })?;

                let alloc_fn = alloc_func
                    .typed::<i32, i32>(&caller)
                    .map_err(|e| WasmiError::new(format!("typing __wafer_alloc: {e}")))?;

                let ptr = alloc_fn
                    .call(&mut caller, encoded.len() as i32)
                    .map_err(|e| {
                        WasmiError::new(format!("__wafer_alloc({}): {e}", encoded.len()))
                    })?;

                memory
                    .write(&mut caller, ptr as usize, &encoded)
                    .map_err(|_| {
                        WasmiError::new("writing attachment bytes to guest memory".to_string())
                    })?;

                Ok(pack_ptr_len(ptr as u32, encoded.len() as u32))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_lookup_attachment: {e}")))?;

    // __wafer_host_load_asset(id_ptr, id_len) -> i32
    //
    // Reads the asset id from guest memory, stashes it in pending_load_asset,
    // and traps. The resume loop in `call_guest_resumable` drives the
    // registered `LoadAssetCallback` and resumes the guest with the resolved
    // i32 status code as the return value. (Unlike `__wafer_host_call_block`,
    // this host function has no phase-2 re-entry — the resume value IS the
    // return value in wasmi's resumable-call API.)
    //
    // Status codes:
    //   0 = Ready, 1 = Pending, 2 = Failed
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_load_asset",
            |mut caller: Caller<WasmiHostState>,
             id_ptr: i32,
             id_len: i32|
             -> Result<i32, WasmiError> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| WasmiError::new("guest has no exported memory"))?;

                let id_buf = read_guest_slice(&caller, memory, id_ptr, id_len)
                    .map_err(|e| WasmiError::new(format!("reading asset id: {e}")))?;
                let asset_id = String::from_utf8(id_buf)
                    .map_err(|e| WasmiError::new(format!("asset id not UTF-8: {e}")))?;

                caller.data_mut().pending_load_asset = Some(asset_id);
                Err(WasmiError::host(LoadAssetTrap))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_load_asset: {e}")))?;

    // ---- WASI stubs (wasi_snapshot_preview1) ----

    // fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            |mut caller: Caller<WasmiHostState>,
             _fd: i32,
             _iovs_ptr: i32,
             _iovs_len: i32,
             nwritten_ptr: i32|
             -> i32 {
                // Discard output. Write 0 to nwritten. A bad nwritten pointer is
                // a guest bug — report it instead of claiming success.
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    if memory
                        .write(&mut caller, nwritten_ptr as usize, &0u32.to_le_bytes())
                        .is_err()
                    {
                        return WASI_ERRNO_FAULT;
                    }
                }
                WASI_ERRNO_SUCCESS
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking fd_write stub: {e}")))?;

    // proc_exit(code)
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "proc_exit",
            |_caller: Caller<WasmiHostState>, code: i32| -> Result<(), WasmiError> {
                Err(WasmiError::host(ProcExitTrap { code }))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking proc_exit stub: {e}")))?;

    // environ_sizes_get(argc_ptr, argv_buf_size_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_sizes_get",
            |mut caller: Caller<WasmiHostState>, argc_ptr: i32, argv_buf_size_ptr: i32| -> i32 {
                let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return WASI_ERRNO_FAULT;
                };
                if memory
                    .write(&mut caller, argc_ptr as usize, &0u32.to_le_bytes())
                    .is_err()
                    || memory
                        .write(&mut caller, argv_buf_size_ptr as usize, &0u32.to_le_bytes())
                        .is_err()
                {
                    return WASI_ERRNO_FAULT;
                }
                WASI_ERRNO_SUCCESS
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking environ_sizes_get stub: {e}")))?;

    // environ_get(argv_ptr, argv_buf_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_get",
            |_caller: Caller<WasmiHostState>, _argv_ptr: i32, _argv_buf_ptr: i32| -> i32 {
                // Zero environment variables — nothing to write, always succeeds.
                WASI_ERRNO_SUCCESS
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking environ_get stub: {e}")))?;

    // args_sizes_get(argc_ptr, argv_buf_size_ptr) -> errno
    // TinyGo WASM runtime imports this to enumerate command-line arguments.
    // We expose zero arguments.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "args_sizes_get",
            |mut caller: Caller<WasmiHostState>, argc_ptr: i32, argv_buf_size_ptr: i32| -> i32 {
                let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return WASI_ERRNO_FAULT;
                };
                if memory
                    .write(&mut caller, argc_ptr as usize, &0u32.to_le_bytes())
                    .is_err()
                    || memory
                        .write(&mut caller, argv_buf_size_ptr as usize, &0u32.to_le_bytes())
                        .is_err()
                {
                    return WASI_ERRNO_FAULT;
                }
                WASI_ERRNO_SUCCESS
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking args_sizes_get stub: {e}")))?;

    // args_get(argv_ptr, argv_buf_ptr) -> errno
    // TinyGo WASM runtime imports this to read command-line arguments.
    // We expose zero arguments so this is a no-op.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "args_get",
            |_caller: Caller<WasmiHostState>, _argv_ptr: i32, _argv_buf_ptr: i32| -> i32 {
                // Zero command-line arguments — nothing to write, always succeeds.
                WASI_ERRNO_SUCCESS
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking args_get stub: {e}")))?;

    // clock_time_get(id, precision, time_ptr) -> errno
    // WASI clock IDs (we only differentiate REALTIME vs. anything-else):
    //   0 = CLOCK_REALTIME      → wall clock, ns since UNIX epoch
    //   1 = CLOCK_MONOTONIC     → ns since an arbitrary fixed point
    //   2 = CLOCK_PROCESS_CPUTIME_ID
    //   3 = CLOCK_THREAD_CPUTIME_ID
    // We use `web_time` so this works both natively and when the runtime is
    // itself compiled to WASM and embedded in a browser (the browser build): on
    // wasm32 the call delegates to `Date.now()` / `Performance.now()`.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "clock_time_get",
            |mut caller: Caller<WasmiHostState>, id: i32, _precision: i64, time_ptr: i32| -> i32 {
                let nanos: u64 = if id == 0 {
                    web_time::SystemTime::now()
                        .duration_since(web_time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_nanos() as u64)
                } else {
                    // For monotonic/CPU-time IDs, fall back to an Instant-based
                    // counter relative to the runtime's own start. This is the
                    // best we can do without process boot-time, and it is
                    // monotonic — sufficient for chrono's elapsed-time paths.
                    static START: std::sync::OnceLock<web_time::Instant> =
                        std::sync::OnceLock::new();
                    let start = START.get_or_init(web_time::Instant::now);
                    web_time::Instant::now().duration_since(*start).as_nanos() as u64
                };
                let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return WASI_ERRNO_FAULT;
                };
                if memory
                    .write(&mut caller, time_ptr as usize, &nanos.to_le_bytes())
                    .is_err()
                {
                    return WASI_ERRNO_FAULT;
                }
                WASI_ERRNO_SUCCESS
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking clock_time_get stub: {e}")))?;

    // sched_yield() -> errno
    // Pure-Rust guests (e.g. blocks pulling in `scraper`/`ahash`, whose spin-init
    // and contention back-off paths reference `std::thread::yield_now`) import
    // this. The wasmi guest is single-threaded and runs to completion on one
    // host thread, so there is no other thread to yield to — the yield is a
    // semantic no-op. Return success, mirroring `environ_get`.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "sched_yield",
            |_caller: Caller<WasmiHostState>| -> i32 {
                // Single-threaded guest: nothing to yield to. Always succeeds.
                WASI_ERRNO_SUCCESS
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking sched_yield stub: {e}")))?;

    // random_get(buf_ptr, buf_len) -> errno
    // TinyGo WASM runtime imports this for crypto/rand and map seed initialisation.
    // We fill the buffer with real random bytes via getrandom.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "random_get",
            |mut caller: Caller<WasmiHostState>, buf_ptr: i32, buf_len: i32| -> i32 {
                let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return WASI_ERRNO_FAULT;
                };
                // `buf` is filled by the host RNG, not read from guest memory —
                // but it's still sized off the guest-controlled `buf_len`, so
                // the same pre-allocation range check applies (see
                // `checked_guest_range`'s doc comment): reject a negative or
                // out-of-bounds length before allocating the fill buffer.
                let Ok((_, len)) = checked_guest_range(&caller, memory, buf_ptr, buf_len) else {
                    return WASI_ERRNO_FAULT;
                };
                let mut buf = vec![0u8; len];
                if getrandom::getrandom(&mut buf).is_err() {
                    // RNG failure: never hand the guest a zero-filled buffer with
                    // a success errno — that would silently produce non-random
                    // seeds. Report the failure so the guest can react.
                    warn!("getrandom failed in WASI random_get");
                    return WASI_ERRNO_IO;
                }
                if memory.write(&mut caller, buf_ptr as usize, &buf).is_err() {
                    return WASI_ERRNO_FAULT;
                }
                WASI_ERRNO_SUCCESS
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking random_get stub: {e}")))?;

    Ok(linker)
}
