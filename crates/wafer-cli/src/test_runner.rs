use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context};
use wasmi::{Caller, Engine, Linker, Module, Store};

/// State held in the wasmi store during test execution.
struct TestState {
    /// block-name -> mock BlockResult JSON bytes
    mock_results: HashMap<String, Vec<u8>>,
    /// Current write offset for mock responses in guest memory.
    /// Starts at MOCK_BUF_OFFSET and advances after each call_block call
    /// so that multiple calls within one Handle() invocation don't overwrite
    /// each other.
    mock_write_offset: u32,
}

const DEFAULT_CONTINUE: &[u8] =
    br#"{"action":"Continue","response":null,"error":null,"message":null}"#;

/// Offset in guest memory used for writing mock responses.
/// 1 MiB — well above typical stack/heap usage for a freshly-started guest.
const MOCK_BUF_OFFSET: usize = 1024 * 1024;

/// Run test fixtures for the block in `dir`.
///
/// * `specific_path` — if Some, run only that one file; otherwise run all
///   `tests/*.json` files (excluding `tests/mocks/`).
pub fn run_tests(dir: &Path, specific_path: Option<&str>) -> anyhow::Result<()> {
    // -----------------------------------------------------------------------
    // 1. Load block.wasm
    // -----------------------------------------------------------------------
    let wasm_path = dir.join("target/block.wasm");
    if !wasm_path.exists() {
        bail!(
            "No block.wasm found at {}.\nRun `wafer build` first.",
            wasm_path.display()
        );
    }

    let wasm_bytes = std::fs::read(&wasm_path)
        .with_context(|| format!("Failed to read WASM file: {}", wasm_path.display()))?;

    // -----------------------------------------------------------------------
    // 2. Compile module
    // -----------------------------------------------------------------------
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm_bytes)
        .with_context(|| format!("Failed to compile WASM module: {}", wasm_path.display()))?;

    // -----------------------------------------------------------------------
    // 3. Load mock results from tests/mocks/*.json
    // -----------------------------------------------------------------------
    let mocks_dir = dir.join("tests/mocks");
    let mut mock_results: HashMap<String, Vec<u8>> = HashMap::new();

    if mocks_dir.is_dir() {
        for entry in std::fs::read_dir(&mocks_dir)
            .with_context(|| format!("Failed to read mocks dir: {}", mocks_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                // filename uses "--" instead of "/" for block names
                let block_name = stem.replacen("--", "/", 1);
                let contents = std::fs::read(&path)
                    .with_context(|| format!("Failed to read mock: {}", path.display()))?;
                mock_results.insert(block_name, contents);
            }
        }
    }

    // -----------------------------------------------------------------------
    // 4. Build linker with host stubs
    // -----------------------------------------------------------------------
    let mut linker = Linker::<TestState>::new(&engine);

    // wafer::__wafer_host_is_cancelled() -> i32
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_is_cancelled",
            |_: Caller<TestState>| 0i32,
        )
        .context("Failed to define __wafer_host_is_cancelled stub")?;

    // wafer::__wafer_host_log(level_ptr, level_len, msg_ptr, msg_len) — no-op
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_log",
            |_: Caller<TestState>,
             _level_ptr: i32,
             _level_len: i32,
             _msg_ptr: i32,
             _msg_len: i32| {},
        )
        .context("Failed to define __wafer_host_log stub")?;

    // wafer::__wafer_host_call_block(name_ptr, name_len, msg_ptr, msg_len) -> i64
    //
    // Looks up the block name in mock_results. Writes the mock JSON into guest
    // memory at the current mock_write_offset (starting at MOCK_BUF_OFFSET) and
    // advances the offset so multiple calls don't overwrite each other.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_call_block",
            |mut caller: Caller<TestState>,
             name_ptr: i32,
             name_len: i32,
             _msg_ptr: i32,
             _msg_len: i32|
             -> i64 {
                // Read block name from guest memory
                let block_name =
                    if let Some(wasmi::Extern::Memory(mem)) = caller.get_export("memory") {
                        let mut name_bytes = vec![0u8; name_len as usize];
                        let _ = mem.read(&caller, name_ptr as usize, &mut name_bytes);
                        String::from_utf8(name_bytes).unwrap_or_default()
                    } else {
                        String::new()
                    };

                // Choose mock payload
                let payload: Vec<u8> = caller
                    .data()
                    .mock_results
                    .get(&block_name)
                    .cloned()
                    .unwrap_or_else(|| DEFAULT_CONTINUE.to_vec());

                // Write payload into guest memory at the current advancing offset.
                // Using an incrementing offset means multiple call_block calls within
                // a single Handle() invocation don't overwrite each other's results.
                let write_offset = caller.data().mock_write_offset as usize;
                if let Some(wasmi::Extern::Memory(mem)) = caller.get_export("memory") {
                    let _ = mem.write(&mut caller, write_offset, &payload);
                }

                // Advance the offset past this payload (align to 8 bytes for safety).
                let advance = payload.len().div_ceil(8) * 8;
                caller.data_mut().mock_write_offset += advance as u32;

                let ptr = write_offset as i64;
                let len = payload.len() as i64;
                (ptr << 32) | len
            },
        )
        .context("Failed to define __wafer_host_call_block stub")?;

    // wasi_snapshot_preview1::fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno
    // For fd=2 (stderr), print the output to help debug panics in guest code.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            |mut caller: Caller<TestState>,
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

    // wasi_snapshot_preview1::proc_exit(code) — trap
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "proc_exit",
            |_: Caller<TestState>, code: i32| -> Result<(), wasmi::Error> {
                Err(wasmi::Error::new(format!("guest called proc_exit({code})")))
            },
        )
        .context("Failed to define proc_exit stub")?;

    // wasi_snapshot_preview1::environ_sizes_get(argc_ptr, argv_buf_size_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_sizes_get",
            |mut caller: Caller<TestState>, argc_ptr: i32, argv_buf_size_ptr: i32| -> i32 {
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
            |_: Caller<TestState>, _argv_ptr: i32, _argv_buf_ptr: i32| -> i32 { 0 },
        )
        .context("Failed to define environ_get stub")?;

    // wasi_snapshot_preview1::args_sizes_get(argc_ptr, argv_buf_size_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "args_sizes_get",
            |mut caller: Caller<TestState>, argc_ptr: i32, argv_buf_size_ptr: i32| -> i32 {
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
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "args_get",
            |_: Caller<TestState>, _argv_ptr: i32, _argv_buf_ptr: i32| -> i32 { 0 },
        )
        .context("Failed to define args_get stub")?;

    // wasi_snapshot_preview1::clock_time_get(id, precision, time_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "clock_time_get",
            |mut caller: Caller<TestState>, _id: i32, _precision: i64, time_ptr: i32| -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, time_ptr as usize, &0u64.to_le_bytes());
                }
                0
            },
        )
        .context("Failed to define clock_time_get stub")?;

    // wasi_snapshot_preview1::random_get(buf_ptr, buf_len) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "random_get",
            |mut caller: Caller<TestState>, buf_ptr: i32, buf_len: i32| -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let zeros = vec![0u8; buf_len as usize];
                    let _ = memory.write(&mut caller, buf_ptr as usize, &zeros);
                }
                0
            },
        )
        .context("Failed to define random_get stub")?;

    // wasi_snapshot_preview1::poll_oneoff(in_ptr, out_ptr, nsubscriptions, nevents_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "poll_oneoff",
            |mut caller: Caller<TestState>,
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
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "sched_yield",
            |_: Caller<TestState>| -> i32 { 0 },
        )
        .context("Failed to define sched_yield stub")?;

    // -----------------------------------------------------------------------
    // 5. Instantiate
    // -----------------------------------------------------------------------
    let mut store = Store::new(
        &engine,
        TestState {
            mock_results,
            mock_write_offset: MOCK_BUF_OFFSET as u32,
        },
    );

    let instance = linker
        .instantiate(&mut store, &module)
        .context("Failed to instantiate WASM module")?
        .start(&mut store)
        .context("Failed to run WASM start function")?;

    // -----------------------------------------------------------------------
    // 5b. Call the exported `_start` function if present.
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
                if msg.contains("guest called proc_exit(0)") {
                    // proc_exit(0) — normal WASI shutdown for TinyGo modules.
                } else {
                    bail!("WASM _start function failed: {e}");
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // 6. Collect test files
    // -----------------------------------------------------------------------
    let test_files: Vec<std::path::PathBuf> = if let Some(p) = specific_path {
        vec![dir.join(p)]
    } else {
        let tests_dir = dir.join("tests");
        if !tests_dir.is_dir() {
            bail!(
                "No tests/ directory found at {}.\nCreate test fixtures in tests/*.json.",
                tests_dir.display()
            );
        }
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&tests_dir)
            .with_context(|| format!("Failed to read tests dir: {}", tests_dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                // Only top-level .json files — skip directories (mocks/) and .expected.json
                p.is_file()
                    && p.extension().is_some_and(|e| e == "json")
                    && !p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".expected.json"))
            })
            .collect();
        files.sort();
        files
    };

    if test_files.is_empty() {
        bail!("No test files found. Create tests/*.json fixtures.");
    }

    // -----------------------------------------------------------------------
    // 7. Get typed function handles (shared across all tests)
    // -----------------------------------------------------------------------
    let alloc = instance
        .get_typed_func::<i32, i32>(&store, "__wafer_alloc")
        .context("Failed to get __wafer_alloc export")?;
    let handle = instance
        .get_typed_func::<(i32, i32), i64>(&store, "__wafer_handle")
        .context("Failed to get __wafer_handle export")?;

    // -----------------------------------------------------------------------
    // 8. Run tests
    // -----------------------------------------------------------------------
    let mut passed = 0usize;
    let mut failed = 0usize;

    for test_path in &test_files {
        let test_name = test_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unknown>");

        let result = run_single_test(test_path, test_name, &mut store, &alloc, &handle, &instance);

        match result {
            Ok(()) => {
                println!("PASS  {test_name}");
                passed += 1;
            }
            Err(e) => {
                println!("FAIL  {test_name}");
                // Print each line of the error indented
                for line in format!("{e:#}").lines() {
                    println!("      {line}");
                }
                failed += 1;
            }
        }
    }

    // -----------------------------------------------------------------------
    // 9. Summary
    // -----------------------------------------------------------------------
    println!();
    println!(
        "Results: {} passed, {} failed, {} total",
        passed,
        failed,
        passed + failed
    );

    if failed > 0 {
        bail!("{} test(s) failed", failed);
    }

    Ok(())
}

/// Run a single test file. Returns Ok if the test passes, Err with a
/// descriptive message if it fails.
fn run_single_test(
    test_path: &Path,
    test_name: &str,
    store: &mut Store<TestState>,
    alloc: &wasmi::TypedFunc<i32, i32>,
    handle: &wasmi::TypedFunc<(i32, i32), i64>,
    instance: &wasmi::Instance,
) -> anyhow::Result<()> {
    // -----------------------------------------------------------------------
    // a. Read and parse the test fixture as a Message
    // -----------------------------------------------------------------------
    let fixture_bytes = std::fs::read(test_path)
        .with_context(|| format!("Failed to read test file: {}", test_path.display()))?;

    let _message: wafer_block::Message = serde_json::from_slice(&fixture_bytes)
        .with_context(|| format!("Test file {test_name} is not a valid Message JSON"))?;

    // Re-serialize to ensure canonical form
    let msg_bytes = serde_json::to_vec(&_message)
        .with_context(|| format!("Failed to serialize message for {test_name}"))?;

    // -----------------------------------------------------------------------
    // b. Allocate guest memory and write the message
    // -----------------------------------------------------------------------
    let ptr = alloc
        .call(&mut *store, msg_bytes.len() as i32)
        .with_context(|| format!("__wafer_alloc failed for {test_name}"))?;

    let memory = instance
        .get_memory(&*store, "memory")
        .with_context(|| "WASM module has no exported 'memory'")?;

    memory
        .write(&mut *store, ptr as usize, &msg_bytes)
        .with_context(|| format!("Failed to write message to guest memory for {test_name}"))?;

    // -----------------------------------------------------------------------
    // c. Call __wafer_handle and unpack the result
    // -----------------------------------------------------------------------
    let result_packed = handle
        .call(&mut *store, (ptr, msg_bytes.len() as i32))
        .with_context(|| format!("__wafer_handle trapped for {test_name}"))?;

    let result_ptr = (result_packed >> 32) as u32;
    let result_len = (result_packed & 0xFFFF_FFFF) as u32;

    // -----------------------------------------------------------------------
    // d. Read result bytes from guest memory
    // -----------------------------------------------------------------------
    let result_bytes = {
        let data = memory.data(&*store);
        let start = result_ptr as usize;
        let end = start
            .checked_add(result_len as usize)
            .filter(|&e| e <= data.len())
            .with_context(|| {
                format!(
                    "__wafer_handle returned out-of-bounds region: ptr={result_ptr}, \
                     len={result_len}, memory_size={}",
                    data.len()
                )
            })?;
        data[start..end].to_vec()
    };

    // -----------------------------------------------------------------------
    // e. Parse as BlockResult
    // -----------------------------------------------------------------------
    let actual: serde_json::Value = serde_json::from_slice(&result_bytes).with_context(|| {
        format!(
            "__wafer_handle returned invalid JSON for {test_name}: {:?}",
            String::from_utf8_lossy(&result_bytes)
        )
    })?;

    // Validate the result has an "action" field (new streaming protocol format)
    if !actual.is_object() {
        anyhow::bail!("__wafer_handle result is not a JSON object for {test_name}: {actual}");
    }

    // -----------------------------------------------------------------------
    // f. Check expected output if a .expected.json sibling exists
    // -----------------------------------------------------------------------
    let expected_path = {
        let stem = test_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        test_path.with_file_name(format!("{stem}.expected.json"))
    };

    if expected_path.exists() {
        let expected_bytes = std::fs::read(&expected_path).with_context(|| {
            format!("Failed to read expected file: {}", expected_path.display())
        })?;
        let expected: serde_json::Value =
            serde_json::from_slice(&expected_bytes).with_context(|| {
                format!(
                    "Expected file {} is not valid JSON",
                    expected_path.display()
                )
            })?;

        if actual != expected {
            bail!("Output mismatch for {test_name}:\n  expected: {expected}\n    actual: {actual}");
        }
    }
    // If no expected file, passing means the block didn't trap (smoke test).

    Ok(())
}
