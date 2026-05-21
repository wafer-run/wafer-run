//! Regression test for the `::wafer_block::register_static_block!` path-resolution
//! bug in `#[wafer_block]`.
//!
//! The proc macro emits a call to `::wafer_block::register_static_block!`. On
//! `wasm32-*` the macro arm is a no-op, but the absolute path lookup still
//! fires at macro-expansion time — which fails (E0433) when the consumer crate
//! has no `wafer-block` dep in its graph. Real skill-block crates (e.g.
//! `gizza-ai/clock`) only depend on `wafer-sdk` + `wafer-block`, so the new
//! `::wafer_block::...` path resolves cleanly with no `wafer-run` dep needed.
//!
//! This test compiles a minimal skill-block fixture for `wasm32-wasip1` and
//! asserts the build succeeds. The fixture lives in
//! `tests/skill-block-without-wafer-run-dep/` and declares its own `[workspace]`
//! so it is not picked up by the parent `wafer-run` workspace.
//!
//! Skipped automatically when the `wasm32-wasip1` toolchain is not installed
//! locally (CI must keep that toolchain available for the test to be
//! meaningful).

use std::{path::PathBuf, process::Command};

#[test]
fn skill_block_without_wafer_run_dep_compiles_for_wasm32() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_dir = manifest_dir
        .join("tests")
        .join("skill-block-without-wafer-run-dep");

    assert!(
        fixture_dir.join("Cargo.toml").is_file(),
        "fixture Cargo.toml missing at {}",
        fixture_dir.display()
    );

    // Use a target dir under OUT_DIR-like scratch so this doesn't pollute
    // the parent workspace's `target/`.
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("target"))
        .join("skill-block-regression");

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    let output = Command::new(&cargo)
        .current_dir(&fixture_dir)
        .args(["build", "--release", "--target", "wasm32-wasip1"])
        .env("CARGO_TARGET_DIR", &target_dir)
        // Ensure we don't leak the parent workspace's RUSTFLAGS, which can
        // include host-arch-specific link args that break wasm32 builds.
        .env_remove("RUSTFLAGS")
        .output()
        .expect("failed to spawn cargo");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Skip if the wasm32-wasip1 toolchain isn't installed on this host.
    if !output.status.success()
        && (stderr.contains("target may not be installed")
            || stderr.contains("the `wasm32-wasip1` target may not be installed")
            || stderr.contains("can't find crate for `core`"))
    {
        eprintln!("skipping: wasm32-wasip1 toolchain not installed\nstderr:\n{stderr}");
        return;
    }

    assert!(
        output.status.success(),
        "fixture failed to build for wasm32-wasip1 — this is the regression \
         the cfg-gate in wafer-block-macro is supposed to prevent.\n\n\
         stdout:\n{stdout}\n\nstderr:\n{stderr}",
    );
}
