//! `wafer package` and `wafer test` speak the core ABI the runtime speaks
//! (`wafer_block::abi::CoreCodec`): a block built with the current
//! `#[wafer_block]` macro (v2 MessagePack) packages and tests, and a guest
//! declaring a version the runtime does not speak is refused by both, with
//! the runtime's message. The legacy v1 JSON guest is covered by
//! `test_abi_gate.rs`.
//!
//! The macro fixture lives at `tests/fixtures/macro_block/`; it is its own
//! `[workspace]` and is built by `scripts/build-fixtures.sh`. See
//! [`macro_block_wasm`] for the exact command.

#[path = "util/mod.rs"]
mod util;

use std::path::{Path, PathBuf};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wafer")
}

/// The prebuilt macro fixture. Build it with:
///
/// ```bash
/// cargo build --target wasm32-wasip1 --release \
///     --manifest-path crates/wafer-cli/tests/fixtures/macro_block/Cargo.toml
/// ```
fn macro_block_wasm() -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/macro_block/target/wasm32-wasip1/release/macro_block.wasm");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "failed to read the macro-block fixture at {}: {e}\n\
             Did you build it first? ./scripts/build-fixtures.sh, or\n  \
             cargo build --target wasm32-wasip1 --release \\\n    \
             --manifest-path crates/wafer-cli/tests/fixtures/macro_block/Cargo.toml",
            p.display()
        )
    })
}

/// A project dir holding `wasm` as `target/block.wasm` and a `wafer.toml`
/// for `{org}/{name}`.
fn project(dir: &Path, org: &str, name: &str, wasm: &[u8]) {
    std::fs::write(
        dir.join("wafer.toml"),
        format!("[package]\norg = \"{org}\"\nname = \"{name}\"\nversion = \"0.1.0\"\nabi = 1\n"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::write(dir.join("target/block.wasm"), wasm).unwrap();
}

/// A guest declaring core ABI v3, a version neither the runtime nor the CLI
/// speaks: `__wafer_abi_version` returns 3. Hand-written because the
/// unsupported guest is the module under test; no SDK can build one.
fn v3_block_wasm(full_name: &str) -> Vec<u8> {
    util::wat_block(
        full_name,
        "",
        "{}",
        r#"(func (export "__wafer_abi_version") (result i32) i32.const 3)"#,
    )
}

/// The JSON of a `GuestResult::error`, as a v1 guest writes it and as
/// `wafer test` writes a result for comparison.
fn error_result_json() -> String {
    serde_json::to_string(&wafer_block::abi::GuestResult::error(
        wafer_block::WaferError::new(wafer_block::ErrorCode::Internal, "boom"),
    ))
    .unwrap()
}

fn wafer(dir: &Path, arg: &str) -> (bool, String, String) {
    let out = std::process::Command::new(bin())
        .current_dir(dir)
        .arg(arg)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn package_accepts_a_macro_built_block() {
    let tmp = tempfile::tempdir().unwrap();
    project(tmp.path(), "acme", "macro-block", &macro_block_wasm());

    let (ok, stdout, stderr) = wafer(tmp.path(), "package");
    assert!(
        ok,
        "wafer package must decode a v2 block's BlockInfo;\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("Packaged  acme/macro-block v0.1.0"),
        "{stdout}"
    );
    assert!(
        tmp.path()
            .join("target/wafer/macro-block-0.1.0.wafer")
            .is_file(),
        "tarball missing"
    );
}

/// The name check compares wafer.toml with the name decoded from the guest,
/// so a mismatch report naming the macro's `name` proves the BlockInfo was
/// decoded, not merely that validation was skipped.
#[test]
fn package_reports_the_decoded_name_of_a_macro_built_block() {
    let tmp = tempfile::tempdir().unwrap();
    project(tmp.path(), "acme", "other", &macro_block_wasm());

    let (ok, _stdout, stderr) = wafer(tmp.path(), "package");
    assert!(!ok, "a name mismatch must fail packaging");
    assert!(
        stderr.contains(r#"but __wafer_info() returned "acme/macro-block""#),
        "{stderr}"
    );
}

#[test]
fn test_runs_a_macro_built_block_against_its_expected_result() {
    let tmp = tempfile::tempdir().unwrap();
    project(tmp.path(), "acme", "macro-block", &macro_block_wasm());
    std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
    // "hello" as body bytes; the block echoes it.
    std::fs::write(
        tmp.path().join("tests/echo.json"),
        r#"{"kind":"run","meta":[],"data":[104,101,108,108,111]}"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("tests/echo.expected.json"),
        r#"{"action":"Respond","response":{"data":[104,101,108,108,111],"meta":[]},"error":null,"message":null}"#,
    )
    .unwrap();

    let (ok, stdout, stderr) = wafer(tmp.path(), "test");
    assert!(
        ok,
        "wafer test must send a v2 frame and decode the v2 result;\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("PASS  echo.json"), "{stdout}");
}

/// A guest answering `Error` fails its fixture when no `.expected.json` says
/// the error is expected. The guest is a v1 one whose handle result is
/// always an `Error`.
#[test]
fn test_fails_a_fixture_whose_result_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("target")).unwrap();
    std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
    std::fs::write(
        tmp.path().join("target/block.wasm"),
        util::wat_block("acme/widget", "", &error_result_json(), ""),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("tests/smoke.json"),
        r#"{"kind":"run","meta":[]}"#,
    )
    .unwrap();

    let (ok, stdout, _stderr) = wafer(tmp.path(), "test");
    assert!(!ok, "an Error result must fail the run: {stdout}");
    assert!(stdout.contains("FAIL  smoke.json"), "{stdout}");
    assert!(stdout.contains("returned action Error"), "{stdout}");

    // With an `.expected.json` naming that error, the same result passes.
    std::fs::write(
        tmp.path().join("tests/smoke.expected.json"),
        error_result_json(),
    )
    .unwrap();
    let (ok, stdout, stderr) = wafer(tmp.path(), "test");
    assert!(
        ok,
        "an expected Error must pass;\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("PASS  smoke.json"), "{stdout}");
}

const V3_REFUSAL: &str = "guest declares core ABI v3; this host supports v1 (implicit) and v2";

#[test]
fn package_refuses_an_unsupported_abi_version() {
    let tmp = tempfile::tempdir().unwrap();
    project(tmp.path(), "acme", "widget", &v3_block_wasm("acme/widget"));

    let (ok, stdout, stderr) = wafer(tmp.path(), "package");
    assert!(!ok, "a v3 guest must not package: {stdout}");
    assert!(stderr.contains(V3_REFUSAL), "{stderr}");
}

#[test]
fn test_refuses_an_unsupported_abi_version() {
    let tmp = tempfile::tempdir().unwrap();
    project(tmp.path(), "acme", "widget", &v3_block_wasm("acme/widget"));
    std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
    std::fs::write(
        tmp.path().join("tests/smoke.json"),
        r#"{"kind":"run","meta":[]}"#,
    )
    .unwrap();

    let (ok, stdout, stderr) = wafer(tmp.path(), "test");
    assert!(!ok, "a v3 guest must not be tested: {stdout}");
    assert!(stderr.contains(V3_REFUSAL), "{stderr}");
}

/// v1 stays supported: a legacy JSON guest (no `__wafer_abi_version`)
/// packages, and its fixture passes against an `.expected.json` in the
/// `GuestResult` shape.
#[test]
fn package_and_test_accept_a_legacy_v1_block() {
    let tmp = tempfile::tempdir().unwrap();
    project(
        tmp.path(),
        "acme",
        "widget",
        &util::v1_block_wasm("acme/widget", ""),
    );
    std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
    std::fs::write(
        tmp.path().join("tests/smoke.json"),
        r#"{"kind":"run","meta":[]}"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("tests/smoke.expected.json"),
        r#"{"action":"Respond","response":{"data":[],"meta":[]},"error":null,"message":null}"#,
    )
    .unwrap();

    let (ok, stdout, stderr) = wafer(tmp.path(), "package");
    assert!(
        ok,
        "a v1 block must package;\nstdout: {stdout}\nstderr: {stderr}"
    );
    let (ok, stdout, stderr) = wafer(tmp.path(), "test");
    assert!(
        ok,
        "a v1 block must test;\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("PASS  smoke.json"), "{stdout}");
}
