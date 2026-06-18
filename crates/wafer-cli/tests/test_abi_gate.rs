//! `wafer test` must accept exactly the host-import set the runtime
//! provides: the current streaming ABI links fine, while the removed legacy
//! `__wafer_host_call_block` import is rejected (mirroring the runtime
//! sentinel in `crates/wafer-run/tests/abi_compat.rs`).

#[path = "util/mod.rs"]
mod util;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wafer")
}

/// Every import the current runtime ABI registers in the `wafer` module
/// (crates/wafer-run/src/wasm/wasmi_loader/imports.rs).
const FULL_CURRENT_ABI_IMPORTS: &str = r#"
  (import "wafer" "__wafer_host_is_cancelled" (func (result i32)))
  (import "wafer" "__wafer_host_log" (func (param i32 i32 i32 i32)))
  (import "wafer" "__wafer_host_stream_init" (func (param i32 i32 i32 i32) (result i64)))
  (import "wafer" "__wafer_host_stream_write_chunk" (func (param i64 i32 i32) (result i32)))
  (import "wafer" "__wafer_host_stream_attach" (func (param i64 i32 i32) (result i32)))
  (import "wafer" "__wafer_host_stream_finish" (func (param i64) (result i32)))
  (import "wafer" "__wafer_host_stream_read_chunk" (func (param i64) (result i64)))
  (import "wafer" "__wafer_host_stream_take_error" (func (param i64) (result i64)))
  (import "wafer" "__wafer_host_stream_close" (func (param i64)))
  (import "wafer" "__wafer_host_lookup_attachment" (func (param i32 i32) (result i64)))
  (import "wafer" "__wafer_host_load_asset" (func (param i32 i32) (result i32)))
"#;

/// Seed a project dir with a fixture and a block.wasm carrying `imports`.
fn seed_project(dir: &std::path::Path, imports: &str) {
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(
        dir.join("target/block.wasm"),
        util::block_wasm("acme/widget", imports),
    )
    .unwrap();
    std::fs::write(dir.join("tests/smoke.json"), r#"{"kind":"run","meta":[]}"#).unwrap();
}

#[test]
fn test_accepts_full_current_runtime_abi() {
    let tmp = tempfile::tempdir().unwrap();
    seed_project(tmp.path(), FULL_CURRENT_ABI_IMPORTS);

    let out = std::process::Command::new(bin())
        .current_dir(tmp.path())
        .arg("test")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "wafer test should link a block importing the full current ABI;\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("PASS  smoke.json"), "{stdout}");
}

#[test]
fn test_rejects_removed_legacy_call_block_import() {
    let tmp = tempfile::tempdir().unwrap();
    seed_project(
        tmp.path(),
        r#"(import "wafer" "__wafer_host_call_block" (func (param i32 i32 i32 i32 i32 i32) (result i64)))"#,
    );

    let out = std::process::Command::new(bin())
        .current_dir(tmp.path())
        .arg("test")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "wafer test must reject the legacy __wafer_host_call_block import the runtime removed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("instantiate"),
        "failure should come from instantiation: {stderr}"
    );
}

#[test]
fn validate_rejects_removed_legacy_call_block_import() {
    // `wafer package` runs validate_wasm — same gate, different entry point.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("wafer.toml"),
        "[package]\norg = \"acme\"\nname = \"widget\"\nversion = \"0.1.0\"\nabi = 1\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("target")).unwrap();
    std::fs::write(
        tmp.path().join("target/block.wasm"),
        util::block_wasm(
            "acme/widget",
            r#"(import "wafer" "__wafer_host_call_block" (func (param i32 i32 i32 i32 i32 i32) (result i64)))"#,
        ),
    )
    .unwrap();

    let out = std::process::Command::new(bin())
        .current_dir(tmp.path())
        .arg("package")
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn test_rejects_wasi_imports_outside_runtime_set() {
    // The runtime registers no poll_oneoff; the CLI must not accept modules
    // the runtime would reject. (sched_yield IS provided — see the runtime's
    // wasmi_loader imports and wasm_stubs — so it must NOT appear here.)
    let tmp = tempfile::tempdir().unwrap();
    seed_project(
        tmp.path(),
        r#"(import "wasi_snapshot_preview1" "poll_oneoff" (func (param i32 i32 i32 i32) (result i32)))"#,
    );

    let out = std::process::Command::new(bin())
        .current_dir(tmp.path())
        .arg("test")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "wafer test must reject WASI imports the runtime does not provide"
    );
}
