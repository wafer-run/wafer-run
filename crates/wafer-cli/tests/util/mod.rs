//! Shared helpers for wafer-cli integration tests.

/// Compile a legacy v1 JSON guest from WAT: a module without
/// `__wafer_abi_version`, which the runtime and the CLI decode as core ABI v1
/// (JSON frames). It exports the full required set (`__wafer_alloc`,
/// `__wafer_info`, `__wafer_handle`, `__wafer_lifecycle`, `memory`).
/// `__wafer_info` returns a real v1 `BlockInfo` with the given
/// `{org}/{block}` name, and `__wafer_handle` a real v1 `GuestResult`
/// (an empty `Respond`, what `wafer_block::abi::GuestResult::respond(vec![])`
/// writes as JSON).
///
/// `extra_imports` is spliced verbatim into the module so tests can probe
/// which host imports the CLI's stub linker accepts (e.g. the stream ABI).
/// A block built with the current `#[wafer_block]` macro speaks v2 instead;
/// that one is the `macro_block` fixture crate (see `core_abi.rs`).
pub fn v1_block_wasm(full_name: &str, extra_imports: &str) -> Vec<u8> {
    wat_block(full_name, extra_imports, V1_RESPOND_EMPTY, "")
}

/// A v1 `GuestResult` JSON: an empty `Respond`.
const V1_RESPOND_EMPTY: &str =
    r#"{"action":"Respond","response":{"data":[],"meta":[]},"error":null,"message":null}"#;

/// The WAT module behind [`v1_block_wasm`], for tests that need a variant of
/// it: `handle_json` is the bytes `__wafer_handle` returns, and
/// `extra_exports` is spliced in after the required exports (for example a
/// `__wafer_abi_version` export, which makes the module a versioned guest).
pub fn wat_block(
    full_name: &str,
    extra_imports: &str,
    handle_json: &str,
    extra_exports: &str,
) -> Vec<u8> {
    let info_json = format!(
        r#"{{"name":"{full_name}","version":"0.1.0","interface":"handler@v1","summary":"test block"}}"#
    );
    // Escape for a WAT data-segment string literal.
    let escape = |s: &str| -> String {
        s.chars()
            .map(|c| match c {
                '"' => "\\\"".to_string(),
                '\\' => "\\\\".to_string(),
                other => other.to_string(),
            })
            .collect()
    };
    let info_offset: i64 = 16;
    let handle_offset: i64 = 2048;
    let info_packed: i64 = (info_offset << 32) | info_json.len() as i64;
    let handle_packed: i64 = (handle_offset << 32) | handle_json.len() as i64;
    let info_escaped = escape(&info_json);
    let handle_escaped = escape(handle_json);
    let wat = format!(
        r#"(module
  {extra_imports}
  (memory (export "memory") 1)
  (data (i32.const {info_offset}) "{info_escaped}")
  (data (i32.const {handle_offset}) "{handle_escaped}")
  (func (export "__wafer_alloc") (param i32) (result i32) i32.const 4096)
  (func (export "__wafer_info") (result i64) i64.const {info_packed})
  (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const {handle_packed})
  (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
  {extra_exports}
)"#
    );
    wat::parse_str(&wat).expect("compile test block WAT")
}
