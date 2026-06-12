//! Shared helpers for wafer-cli integration tests.

/// Compile a minimal-but-valid WAFER block module from WAT. It exports the
/// full required set (`__wafer_alloc`, `__wafer_info`, `__wafer_handle`,
/// `__wafer_lifecycle`, `memory`) and `__wafer_info` returns a real
/// `BlockInfo` JSON payload with the given `{org}/{block}` name.
///
/// `extra_imports` is spliced verbatim into the module so tests can probe
/// which host imports the CLI's stub linker accepts (e.g. the stream ABI).
pub fn block_wasm(full_name: &str, extra_imports: &str) -> Vec<u8> {
    let info_json = format!(
        r#"{{"name":"{full_name}","version":"0.1.0","interface":"handler@v1","summary":"test block"}}"#
    );
    // Escape for a WAT data-segment string literal.
    let escaped: String = info_json
        .chars()
        .map(|c| match c {
            '"' => "\\\"".to_string(),
            '\\' => "\\\\".to_string(),
            other => other.to_string(),
        })
        .collect();
    let info_offset: i64 = 16;
    let packed: i64 = (info_offset << 32) | info_json.len() as i64;
    let wat = format!(
        r#"(module
  {extra_imports}
  (memory (export "memory") 1)
  (data (i32.const {info_offset}) "{escaped}")
  (func (export "__wafer_alloc") (param i32) (result i32) i32.const 4096)
  (func (export "__wafer_info") (result i64) i64.const {packed})
  (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const 0)
  (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
)"#
    );
    wat::parse_str(&wat).expect("compile test block WAT")
}
