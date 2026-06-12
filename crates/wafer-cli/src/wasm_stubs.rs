//! The single wasmi stub linker used by `wafer build`/`wafer package`
//! validation ([`crate::validate`]) and `wafer test` ([`crate::test_runner`]).
//!
//! The import set mirrors the runtime's real host imports
//! (`crates/wafer-run/src/wasm/wasmi_loader/imports.rs`) exactly, so the CLI
//! accepts precisely the modules the runtime will accept:
//!
//! - The full current `wafer` ABI is registered: `is_cancelled`, `log`, the
//!   `stream_*` family (`init`/`write_chunk`/`attach`/`finish`/`read_chunk`/
//!   `take_error`/`close`), `lookup_attachment`, and `load_asset`.
//! - The legacy `__wafer_host_call_block` import is NOT registered — the
//!   runtime deliberately removed it (see
//!   `crates/wafer-run/tests/abi_compat.rs`), so a half-migrated block must
//!   fail `wafer test` the same way it would fail in production.
//! - Only the WASI preview1 imports the runtime registers are stubbed
//!   (`fd_write`, `proc_exit`, `environ_*`, `args_*`, `clock_time_get`,
//!   `random_get`). Anything else (e.g. `poll_oneoff`, `sched_yield`) is
//!   rejected here because the runtime would reject it too.
//!
//! The stubs are inert: nothing in the CLI drives a real stream, so the
//! stream functions return 0 ("permission denied" / "end-of-stream" /
//! "no error" respectively) and `load_asset` reports `Failed`. They exist
//! to satisfy the linker and to keep fixture runs deterministic.

use anyhow::{bail, Context};
use wasmi::{Caller, Engine, Instance, Linker, Memory, Module, Store};

/// Build a linker with the full stub import set (wafer ABI + WASI).
pub fn build_stub_linker<T>(engine: &Engine) -> anyhow::Result<Linker<T>> {
    let mut linker = Linker::<T>::new(engine);
    register_wafer_host_stubs(&mut linker)?;
    register_wasi_stubs(&mut linker)?;
    Ok(linker)
}

/// Register inert stubs for every `wafer`-module host import the runtime
/// provides.
pub fn register_wafer_host_stubs<T>(linker: &mut Linker<T>) -> anyhow::Result<()> {
    // __wafer_host_is_cancelled() -> i32 — never cancelled.
    linker
        .func_wrap("wafer", "__wafer_host_is_cancelled", |_: Caller<T>| 0i32)
        .context("Failed to define __wafer_host_is_cancelled stub")?;

    // __wafer_host_log(level_ptr, level_len, msg_ptr, msg_len) — no-op.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_log",
            |_: Caller<T>, _level_ptr: i32, _level_len: i32, _msg_ptr: i32, _msg_len: i32| {},
        )
        .context("Failed to define __wafer_host_log stub")?;

    // __wafer_host_stream_init(name_ptr, name_len, msg_ptr, msg_len) -> i64
    //
    // Returning 0 models "no stream handle granted"; the CLI never drives a
    // real block-to-block call, so a guest that tries observes an immediate
    // empty stream (write/finish succeed, read_chunk reports end-of-stream).
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_init",
            |_: Caller<T>, _name_ptr: i32, _name_len: i32, _msg_ptr: i32, _msg_len: i32| -> i64 {
                0i64
            },
        )
        .context("Failed to define __wafer_host_stream_init stub")?;

    // __wafer_host_stream_write_chunk(handle, body_ptr, body_len) -> i32 — "ok".
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_write_chunk",
            |_: Caller<T>, _handle: i64, _body_ptr: i32, _body_len: i32| -> i32 { 0i32 },
        )
        .context("Failed to define __wafer_host_stream_write_chunk stub")?;

    // __wafer_host_stream_attach(handle, payload_ptr, payload_len) -> i32 — "ok".
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_attach",
            |_: Caller<T>, _handle: i64, _payload_ptr: i32, _payload_len: i32| -> i32 { 0i32 },
        )
        .context("Failed to define __wafer_host_stream_attach stub")?;

    // __wafer_host_stream_finish(handle) -> i32 — "ok".
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_finish",
            |_: Caller<T>, _handle: i64| -> i32 { 0i32 },
        )
        .context("Failed to define __wafer_host_stream_finish stub")?;

    // __wafer_host_stream_read_chunk(handle) -> i64 — end-of-stream.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_read_chunk",
            |_: Caller<T>, _handle: i64| -> i64 { 0i64 },
        )
        .context("Failed to define __wafer_host_stream_read_chunk stub")?;

    // __wafer_host_stream_take_error(handle) -> i64 — no error recorded.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_take_error",
            |_: Caller<T>, _handle: i64| -> i64 { 0i64 },
        )
        .context("Failed to define __wafer_host_stream_take_error stub")?;

    // __wafer_host_stream_close(handle) — no-op.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_close",
            |_: Caller<T>, _handle: i64| {},
        )
        .context("Failed to define __wafer_host_stream_close stub")?;

    // __wafer_host_lookup_attachment(id_ptr, id_len) -> i64 — "no attachment".
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_lookup_attachment",
            |_: Caller<T>, _id_ptr: i32, _id_len: i32| -> i64 { 0i64 },
        )
        .context("Failed to define __wafer_host_lookup_attachment stub")?;

    // __wafer_host_load_asset(id_ptr, id_len) -> i32
    //
    // Status codes (mirroring the runtime): 0 = Ready, 1 = Pending,
    // 2 = Failed. No asset host exists under the CLI, so report Failed
    // rather than lying with Ready.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_load_asset",
            |_: Caller<T>, _id_ptr: i32, _id_len: i32| -> i32 { 2i32 },
        )
        .context("Failed to define __wafer_host_load_asset stub")?;

    Ok(())
}

/// Register stubs for the WASI preview1 imports the runtime provides.
pub fn register_wasi_stubs<T>(linker: &mut Linker<T>) -> anyhow::Result<()> {
    // fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno
    // For fd 1/2 (stdout/stderr), forward the output to our stderr so guest
    // panics and debug prints are visible during validation/test runs.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            |mut caller: Caller<T>,
             fd: i32,
             iovs_ptr: i32,
             iovs_len: i32,
             nwritten_ptr: i32|
             -> i32 {
                let mut total_written: u32 = 0;
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    for i in 0..iovs_len {
                        let iov_offset = (iovs_ptr as usize) + (i as usize) * 8;
                        let mut buf_ptr_bytes = [0u8; 4];
                        let mut buf_len_bytes = [0u8; 4];
                        let _ = memory.read(&caller, iov_offset, &mut buf_ptr_bytes);
                        let _ = memory.read(&caller, iov_offset + 4, &mut buf_len_bytes);
                        let buf_ptr = u32::from_le_bytes(buf_ptr_bytes) as usize;
                        let buf_len = u32::from_le_bytes(buf_len_bytes) as usize;

                        if buf_len > 0 && (fd == 1 || fd == 2) {
                            let mut buf = vec![0u8; buf_len];
                            let _ = memory.read(&caller, buf_ptr, &mut buf);
                            let text = String::from_utf8_lossy(&buf);
                            eprint!("{text}");
                        }
                        total_written += buf_len as u32;
                    }
                    let _ = memory.write(
                        &mut caller,
                        nwritten_ptr as usize,
                        &total_written.to_le_bytes(),
                    );
                }
                0
            },
        )
        .context("Failed to define fd_write stub")?;

    // proc_exit(code) — trap. `call_start_if_present` treats proc_exit(0)
    // as a normal TinyGo shutdown.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "proc_exit",
            |_: Caller<T>, code: i32| -> Result<(), wasmi::Error> {
                Err(wasmi::Error::new(format!("guest called proc_exit({code})")))
            },
        )
        .context("Failed to define proc_exit stub")?;

    // environ_sizes_get(argc_ptr, argv_buf_size_ptr) -> errno — zero env.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_sizes_get",
            |mut caller: Caller<T>, argc_ptr: i32, argv_buf_size_ptr: i32| -> i32 {
                write_zero_pair(&mut caller, argc_ptr, argv_buf_size_ptr);
                0
            },
        )
        .context("Failed to define environ_sizes_get stub")?;

    // environ_get(argv_ptr, argv_buf_ptr) -> errno — nothing to copy.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_get",
            |_: Caller<T>, _argv_ptr: i32, _argv_buf_ptr: i32| -> i32 { 0 },
        )
        .context("Failed to define environ_get stub")?;

    // args_sizes_get(argc_ptr, argv_buf_size_ptr) -> errno — zero args.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "args_sizes_get",
            |mut caller: Caller<T>, argc_ptr: i32, argv_buf_size_ptr: i32| -> i32 {
                write_zero_pair(&mut caller, argc_ptr, argv_buf_size_ptr);
                0
            },
        )
        .context("Failed to define args_sizes_get stub")?;

    // args_get(argv_ptr, argv_buf_ptr) -> errno — nothing to copy.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "args_get",
            |_: Caller<T>, _argv_ptr: i32, _argv_buf_ptr: i32| -> i32 { 0 },
        )
        .context("Failed to define args_get stub")?;

    // clock_time_get(id, precision, time_ptr) -> errno — epoch (deterministic).
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "clock_time_get",
            |mut caller: Caller<T>, _id: i32, _precision: i64, time_ptr: i32| -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, time_ptr as usize, &0u64.to_le_bytes());
                }
                0
            },
        )
        .context("Failed to define clock_time_get stub")?;

    // random_get(buf_ptr, buf_len) -> errno — zero-filled (deterministic;
    // sufficient for init paths, and fixture runs should be reproducible).
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "random_get",
            |mut caller: Caller<T>, buf_ptr: i32, buf_len: i32| -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let zeros = vec![0u8; buf_len as usize];
                    let _ = memory.write(&mut caller, buf_ptr as usize, &zeros);
                }
                0
            },
        )
        .context("Failed to define random_get stub")?;

    Ok(())
}

/// Write `0u32` to two guest-memory pointers (the `*_sizes_get` shape).
fn write_zero_pair<T>(caller: &mut Caller<T>, a_ptr: i32, b_ptr: i32) {
    if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
        let _ = memory.write(&mut *caller, a_ptr as usize, &0u32.to_le_bytes());
        let _ = memory.write(&mut *caller, b_ptr as usize, &0u32.to_le_bytes());
    }
}

/// Instantiate `module` against the stub linker and run its start section
/// plus the exported `_start` function if present.
///
/// TinyGo WASM modules (wasi target) use an exported `_start` to initialise
/// the Go runtime and call `main()`. Without this call the exported wafer
/// functions trap with `unreachable` because the allocator and goroutine
/// scheduler have not been set up. `_start` ends by calling `proc_exit(0)`,
/// which our stub turns into a trap — that specific trap is treated as a
/// normal shutdown. Rust-compiled blocks (no `_start` export) are
/// unaffected.
pub fn instantiate_and_start<T>(
    linker: &Linker<T>,
    store: &mut Store<T>,
    module: &Module,
) -> anyhow::Result<Instance> {
    let instance = linker
        .instantiate(&mut *store, module)
        .context("Failed to instantiate WASM module")?
        .start(&mut *store)
        .context("Failed to run WASM start function")?;

    if let Ok(start_fn) = instance.get_typed_func::<(), ()>(&mut *store, "_start") {
        if let Err(e) = start_fn.call(&mut *store, ()) {
            let msg = e.to_string();
            if !msg.contains("guest called proc_exit(0)") {
                bail!("WASM _start function failed: {e}");
            }
            // proc_exit(0) — normal WASI shutdown for TinyGo modules.
        }
    }

    Ok(instance)
}

/// Unpack a `(ptr << 32 | len)` i64 returned by a guest export and copy the
/// referenced guest-memory region out, bounds-checked. `what` names the
/// export for error messages.
pub fn read_packed_region<T>(
    memory: &Memory,
    store: &Store<T>,
    packed: i64,
    what: &str,
) -> anyhow::Result<Vec<u8>> {
    let ptr = (packed >> 32) as u32;
    let len = (packed & 0xFFFF_FFFF) as u32;
    let data = memory.data(store);
    let start = ptr as usize;
    let end = start
        .checked_add(len as usize)
        .filter(|&e| e <= data.len())
        .with_context(|| {
            format!(
                "{what} returned out-of-bounds region: ptr={ptr}, len={len}, memory_size={}",
                data.len()
            )
        })?;
    Ok(data[start..end].to_vec())
}
