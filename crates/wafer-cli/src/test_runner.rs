use std::path::Path;

use anyhow::{bail, Context};
use serde::Deserialize;
use wafer_block::abi::{CallFrameRef, CoreCodec, GuestAction, GuestResult};
use wasmi::{Engine, Module, Store};

use crate::wasm_stubs;

/// A test fixture file, `tests/<name>.json`. The `kind` and `meta` fields
/// match `wafer_block::Message`. The optional `data` field carries the body
/// bytes passed as the second argument to the block's `handle(msg, body)`.
///
/// The runner sends `__wafer_handle` the frame the runtime sends
/// ([`wafer_block::abi::CallFrameRef`]) in the codec the module negotiates,
/// and decodes the reply as a [`wafer_block::abi::GuestResult`].
#[derive(Debug, Deserialize)]
struct TestFixture {
    kind: String,
    #[serde(default)]
    meta: Vec<wafer_block::MetaEntry>,
    /// Body bytes passed to `handle(_msg, body)`. Absent = empty body.
    #[serde(default)]
    data: Vec<u8>,
}

/// Run test fixtures for the block in `dir`.
///
/// The module is instantiated against the shared stub linker
/// ([`crate::wasm_stubs`]), which registers exactly the host-import set the
/// runtime provides — including the streaming ABI and excluding the removed
/// legacy `__wafer_host_call_block`. A block that links here links in
/// production too. Its core-ABI codec is negotiated the runtime's way
/// ([`wasm_stubs::negotiate`]), so a guest declaring an unsupported version
/// fails here as it would in production.
///
/// Each `tests/<name>.json` fixture ([`TestFixture`]) is one `handle` call.
/// It passes when the guest returns a well-formed `GuestResult` and:
///
/// * if `tests/<name>.expected.json` exists, the result, written as JSON,
///   equals it. The JSON is the `GuestResult` shape:
///   `{"action":"Respond","response":{"data":[..bytes..],"meta":[]},"error":null,"message":null}`.
///   `action` is `"Respond"`, `"Error"`, `"Drop"` or `"Continue"`;
/// * if it does not, the action is not `"Error"`. A fixture that expects an
///   error says so with an `.expected.json`.
///
/// * `specific_path` — if Some, run only that one file; otherwise run all
///   `tests/*.json` files.
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
    // 3. Build the shared stub linker and instantiate
    // -----------------------------------------------------------------------
    let linker = wasm_stubs::build_stub_linker::<()>(&engine)?;
    let mut store = Store::new(&engine, ());
    let instance = wasm_stubs::instantiate_and_start(&linker, &mut store, &module)?;
    let codec = wasm_stubs::negotiate(&module, &instance, &mut store)?;

    // -----------------------------------------------------------------------
    // 4. Collect test files
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
                // Only top-level .json files — skip directories and .expected.json
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
    // 5. Get typed function handles (shared across all tests)
    // -----------------------------------------------------------------------
    let alloc = instance
        .get_typed_func::<i32, i32>(&store, "__wafer_alloc")
        .context("Failed to get __wafer_alloc export")?;
    let handle = instance
        .get_typed_func::<(i32, i32), i64>(&store, "__wafer_handle")
        .context("Failed to get __wafer_handle export")?;

    // -----------------------------------------------------------------------
    // 6. Run tests
    // -----------------------------------------------------------------------
    let mut passed = 0usize;
    let mut failed = 0usize;

    for test_path in &test_files {
        let test_name = test_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unknown>");

        let result = run_single_test(
            test_path, test_name, codec, &mut store, &alloc, &handle, &instance,
        );

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
    // 7. Summary
    // -----------------------------------------------------------------------
    println!();
    println!(
        "Results: {} passed, {} failed, {} total",
        passed,
        failed,
        passed + failed
    );

    if failed > 0 {
        bail!("{failed} test(s) failed");
    }

    Ok(())
}

/// Run a single test file. Returns Ok if the test passes, Err with a
/// descriptive message if it fails.
fn run_single_test(
    test_path: &Path,
    test_name: &str,
    codec: CoreCodec,
    store: &mut Store<()>,
    alloc: &wasmi::TypedFunc<i32, i32>,
    handle: &wasmi::TypedFunc<(i32, i32), i64>,
    instance: &wasmi::Instance,
) -> anyhow::Result<()> {
    // -----------------------------------------------------------------------
    // a. Read and parse the test fixture.
    //
    // The fixture JSON may include an optional `data` field (Vec<u8>) that
    // carries body bytes for the block's second `handle` argument.
    // -----------------------------------------------------------------------
    let fixture_bytes = std::fs::read(test_path)
        .with_context(|| format!("Failed to read test file: {}", test_path.display()))?;

    let fixture: TestFixture = serde_json::from_slice(&fixture_bytes)
        .with_context(|| format!("Test file {test_name} is not a valid fixture JSON"))?;

    let message = wafer_block::Message {
        kind: fixture.kind,
        meta: fixture.meta,
    };
    let body: Vec<u8> = fixture.data;

    // Encode the frame the runtime sends `__wafer_handle`, in the module's codec.
    let msg_bytes = codec
        .encode(&CallFrameRef(&message, &body))
        .map_err(|e| anyhow::anyhow!("Failed to encode the call frame for {test_name}: {e}"))?;

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
    // c. Call __wafer_handle and read the result bytes
    // -----------------------------------------------------------------------
    let result_packed = handle
        .call(&mut *store, (ptr, msg_bytes.len() as i32))
        .with_context(|| format!("__wafer_handle trapped for {test_name}"))?;

    let result_bytes =
        wasm_stubs::read_packed_region(&memory, store, result_packed, "__wafer_handle")?;

    // -----------------------------------------------------------------------
    // d. Decode the GuestResult with the module's codec
    // -----------------------------------------------------------------------
    let result: GuestResult = codec.decode(&result_bytes).map_err(|e| {
        anyhow::anyhow!(
            "__wafer_handle returned no valid {codec:?} GuestResult for {test_name}: {e}"
        )
    })?;
    let actual = serde_json::to_value(&result)
        .with_context(|| format!("Failed to write the GuestResult of {test_name} as JSON"))?;

    // -----------------------------------------------------------------------
    // e. Check expected output if a .expected.json sibling exists
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
    } else if result.action == GuestAction::Error {
        // No expected file: the call must not fail.
        bail!("__wafer_handle returned action Error for {test_name}: {actual}");
    }

    Ok(())
}
