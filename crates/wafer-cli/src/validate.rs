use std::path::Path;

use anyhow::{bail, Context};
use wasmi::{Engine, Module, Store};

use crate::wasm_stubs;

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
/// Instantiation runs against the shared stub linker
/// ([`crate::wasm_stubs`]), which registers exactly the host-import set the
/// runtime provides — so a module that links here will also link in
/// production, and vice versa.
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
    // 2. Build the shared stub linker and instantiate.
    //    Store data type is `()` — no state needed for validation.
    // -----------------------------------------------------------------------
    let linker = wasm_stubs::build_stub_linker::<()>(&engine)?;
    let mut store = Store::new(&engine, ());
    let instance = wasm_stubs::instantiate_and_start(&linker, &mut store, &module)?;

    // -----------------------------------------------------------------------
    // 3. Check required exports.
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
    // 4. Call __wafer_info() and read the result.
    // -----------------------------------------------------------------------
    let info_fn = instance
        .get_typed_func::<(), i64>(&store, "__wafer_info")
        .context("Failed to get __wafer_info export (wrong signature?)")?;

    let packed = info_fn
        .call(&mut store, ())
        .context("Failed to call __wafer_info")?;

    let memory = instance
        .get_memory(&store, "memory")
        .context("WASM module has no exported 'memory'")?;

    let info_bytes = wasm_stubs::read_packed_region(&memory, &store, packed, "__wafer_info")?;

    // -----------------------------------------------------------------------
    // 5. Deserialize BlockInfo.
    // -----------------------------------------------------------------------
    let info: wafer_block::BlockInfo = serde_json::from_slice(&info_bytes)
        .context("Failed to deserialize BlockInfo from __wafer_info() output")?;

    Ok(info)
}
