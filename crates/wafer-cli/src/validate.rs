use std::path::Path;

use anyhow::{bail, Context};
use wasmi::{Caller, Engine, Linker, Module, Store};

/// Required exports that every WAFER block WASM module must have.
const REQUIRED_EXPORTS: &[&str] = &[
    "__wafer_alloc",
    "__wafer_info",
    "__wafer_handle",
    "__wafer_lifecycle",
    "memory",
];

/// Load a `.wasm` file, verify its exports, call `__wafer_info()`, and return
/// the deserialized [`wafer_block::BlockInfo`].
///
/// This is intentionally a *sync* function — wasmi is sync, and the CLI does
/// not run an async runtime.
pub fn validate_wasm(wasm_path: &Path) -> anyhow::Result<wafer_block::BlockInfo> {
    let wasm_bytes = std::fs::read(wasm_path)
        .with_context(|| format!("Failed to read WASM file: {}", wasm_path.display()))?;

    // -----------------------------------------------------------------------
    // 1. Compile the module.
    // -----------------------------------------------------------------------
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm_bytes)
        .with_context(|| format!("Failed to compile WASM module: {}", wasm_path.display()))?;

    // -----------------------------------------------------------------------
    // 2. Build a linker with stub host imports.
    //    Store data type is `()` — no state needed for validation.
    // -----------------------------------------------------------------------
    let mut linker = Linker::<()>::new(&engine);

    // wafer::__wafer_host_is_cancelled() -> i32
    linker
        .func_wrap("wafer", "__wafer_host_is_cancelled", |_: Caller<()>| 0i32)
        .context("Failed to define __wafer_host_is_cancelled stub")?;

    // wafer::__wafer_host_log(level_ptr, level_len, msg_ptr, msg_len) — no-op
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_log",
            |_: Caller<()>, _level_ptr: i32, _level_len: i32, _msg_ptr: i32, _msg_len: i32| {},
        )
        .context("Failed to define __wafer_host_log stub")?;

    // wafer::__wafer_host_call_block(name_ptr, name_len, msg_ptr, msg_len) -> i64
    // Return 0 — the validator never actually exercises call_block paths.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_call_block",
            |_: Caller<()>, _name_ptr: i32, _name_len: i32, _msg_ptr: i32, _msg_len: i32| 0i64,
        )
        .context("Failed to define __wafer_host_call_block stub")?;

    // wasi_snapshot_preview1::fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            |mut caller: Caller<()>,
             _fd: i32,
             _iovs_ptr: i32,
             _iovs_len: i32,
             nwritten_ptr: i32|
             -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, nwritten_ptr as usize, &0u32.to_le_bytes());
                }
                0
            },
        )
        .context("Failed to define fd_write stub")?;

    // wasi_snapshot_preview1::proc_exit(code) — trap
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "proc_exit",
            |_: Caller<()>, code: i32| -> Result<(), wasmi::Error> {
                Err(wasmi::Error::new(format!("guest called proc_exit({code})")))
            },
        )
        .context("Failed to define proc_exit stub")?;

    // wasi_snapshot_preview1::environ_sizes_get(argc_ptr, argv_buf_size_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_sizes_get",
            |mut caller: Caller<()>, argc_ptr: i32, argv_buf_size_ptr: i32| -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, argc_ptr as usize, &0u32.to_le_bytes());
                    let _ =
                        memory.write(&mut caller, argv_buf_size_ptr as usize, &0u32.to_le_bytes());
                }
                0
            },
        )
        .context("Failed to define environ_sizes_get stub")?;

    // wasi_snapshot_preview1::environ_get(argv_ptr, argv_buf_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_get",
            |_: Caller<()>, _argv_ptr: i32, _argv_buf_ptr: i32| -> i32 { 0 },
        )
        .context("Failed to define environ_get stub")?;

    // wasi_snapshot_preview1::args_sizes_get(argc_ptr, argv_buf_size_ptr) -> errno
    // Required by TinyGo-compiled WASM modules.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "args_sizes_get",
            |mut caller: Caller<()>, argc_ptr: i32, argv_buf_size_ptr: i32| -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, argc_ptr as usize, &0u32.to_le_bytes());
                    let _ =
                        memory.write(&mut caller, argv_buf_size_ptr as usize, &0u32.to_le_bytes());
                }
                0
            },
        )
        .context("Failed to define args_sizes_get stub")?;

    // wasi_snapshot_preview1::args_get(argv_ptr, argv_buf_ptr) -> errno
    // Required by TinyGo-compiled WASM modules. We expose zero arguments.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "args_get",
            |_: Caller<()>, _argv_ptr: i32, _argv_buf_ptr: i32| -> i32 { 0 },
        )
        .context("Failed to define args_get stub")?;

    // wasi_snapshot_preview1::clock_time_get(id, precision, time_ptr) -> errno
    // Required by TinyGo-compiled WASM modules. Returns 0 nanoseconds.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "clock_time_get",
            |mut caller: Caller<()>, _id: i32, _precision: i64, time_ptr: i32| -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, time_ptr as usize, &0u64.to_le_bytes());
                }
                0
            },
        )
        .context("Failed to define clock_time_get stub")?;

    // wasi_snapshot_preview1::random_get(buf_ptr, buf_len) -> errno
    // Required by TinyGo-compiled WASM modules. Fills with zeros (sufficient for init).
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "random_get",
            |mut caller: Caller<()>, buf_ptr: i32, buf_len: i32| -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let zeros = vec![0u8; buf_len as usize];
                    let _ = memory.write(&mut caller, buf_ptr as usize, &zeros);
                }
                0
            },
        )
        .context("Failed to define random_get stub")?;

    // wasi_snapshot_preview1::poll_oneoff(in_ptr, out_ptr, nsubscriptions, nevents_ptr) -> errno
    // Required by boa_engine-compiled WASM modules (JS/TS blocks).
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "poll_oneoff",
            |mut caller: Caller<()>,
             _in_ptr: i32,
             _out_ptr: i32,
             _nsubscriptions: i32,
             nevents_ptr: i32|
             -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, nevents_ptr as usize, &0u32.to_le_bytes());
                }
                0
            },
        )
        .context("Failed to define poll_oneoff stub")?;

    // wasi_snapshot_preview1::sched_yield() -> errno
    // Required by boa_engine-compiled WASM modules (JS/TS blocks).
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "sched_yield",
            |_: Caller<()>| -> i32 { 0 },
        )
        .context("Failed to define sched_yield stub")?;

    // -----------------------------------------------------------------------
    // 3. Instantiate.
    // -----------------------------------------------------------------------
    let mut store = Store::new(&engine, ());

    let instance = linker
        .instantiate(&mut store, &module)
        .context("Failed to instantiate WASM module")?
        .start(&mut store)
        .context("Failed to run WASM start function")?;

    // -----------------------------------------------------------------------
    // 3b. Call the exported `_start` function if present.
    //
    // TinyGo WASM modules (wasi target) use an exported `_start` to
    // initialise the Go runtime and call `main()`. Without this call the
    // exported wafer functions trap with `unreachable` because the allocator
    // and goroutine scheduler have not been set up.
    //
    // `_start` ends by calling `proc_exit(0)` — that traps with our stub
    // error.  We treat any error containing "proc_exit" as normal shutdown.
    // A genuine failure message (e.g. a panic from user code) is re-raised.
    // Rust-compiled blocks (no `_start` export) are unaffected.
    // -----------------------------------------------------------------------
    if let Ok(start_fn) = instance.get_typed_func::<(), ()>(&store, "_start") {
        match start_fn.call(&mut store, ()) {
            Ok(()) => {}
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains("proc_exit") {
                    bail!("WASM _start function failed: {e}");
                }
                // proc_exit(0) — expected for TinyGo/WASI modules.
            }
        }
    }

    // -----------------------------------------------------------------------
    // 4. Check required exports.
    // -----------------------------------------------------------------------
    for export_name in REQUIRED_EXPORTS {
        if instance.get_export(&store, export_name).is_none() {
            bail!(
                "WASM module is missing required export: {export_name}\n\
                 Make sure the block was built with the WAFER SDK and the \
                 #[wafer_block] macro."
            );
        }
    }

    // -----------------------------------------------------------------------
    // 5. Call __wafer_info() and read the result.
    // -----------------------------------------------------------------------
    let info_fn = instance
        .get_typed_func::<(), i64>(&store, "__wafer_info")
        .context("Failed to get __wafer_info export (wrong signature?)")?;

    let packed = info_fn
        .call(&mut store, ())
        .context("Failed to call __wafer_info")?;

    // Unpack the (ptr << 32 | len) i64.
    let ptr = (packed >> 32) as u32;
    let len = (packed & 0xFFFF_FFFF) as u32;

    let memory = instance
        .get_memory(&store, "memory")
        .context("WASM module has no exported 'memory'")?;

    let data = memory.data(&store);
    let start = ptr as usize;
    let end = start
        .checked_add(len as usize)
        .filter(|&e| e <= data.len())
        .with_context(|| {
            format!(
                "__wafer_info returned out-of-bounds region: ptr={ptr}, len={len}, \
                 memory_size={}",
                data.len()
            )
        })?;

    let info_bytes = &data[start..end];

    // -----------------------------------------------------------------------
    // 6. Deserialize BlockInfo.
    // -----------------------------------------------------------------------
    let info: wafer_block::BlockInfo = serde_json::from_slice(info_bytes)
        .context("Failed to deserialize BlockInfo from __wafer_info() output")?;

    Ok(info)
}
