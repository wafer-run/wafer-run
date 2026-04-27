//! Integration tests for the `WaferBuilder` pipeline.
//!
//! These tests exercise the full `build()` path end-to-end, including
//! lockfile loading (Path B). They manipulate process-global state
//! (`HOME`, `WAFER_LOCKFILE`) so they are serialised via `#[serial]`.
//!
//! Fixture strategy: the same MINIMAL_WASM bytes used by the in-`src/`
//! `registry_loader` tests — the magic + version-1 header is sufficient
//! for `WasmiBlock::load_from_bytes` to succeed when the loaded block is
//! only registered, not started/invoked.

use std::{env, fs, path::PathBuf};

use serial_test::serial;
use tempfile::tempdir;
use wafer_run::Wafer;

/// Minimal valid wasm module: "\0asm" magic + version 1 (matches the constant
/// in `registry_loader.rs`'s test module).
const MINIMAL_WASM: &[u8] = &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

/// Seed a fake cache entry under `root/.wafer/cache/{org}/{block}/{version}/`.
/// Writes `wafer.toml` (with separate `org` and `name` fields as the parser
/// expects) and a single `{block}.wasm` (MINIMAL_WASM bytes).
fn seed_cache(home: &std::path::Path, org: &str, block: &str, version: &str) {
    let dir = home
        .join(".wafer")
        .join("cache")
        .join(org)
        .join(block)
        .join(version);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("wafer.toml"),
        format!(
            "[package]\norg = \"{org}\"\nname = \"{block}\"\nversion = \"{version}\"\nabi = 1\n"
        ),
    )
    .unwrap();
    fs::write(dir.join(format!("{block}.wasm")), MINIMAL_WASM).unwrap();
}

// ---------------------------------------------------------------------------
// Test 1: explicit lockfile path loads a block from the cache
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn explicit_lockfile_path_loads_block_from_cache() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().to_path_buf();

    // Seed the cache.
    seed_cache(&home, "acme", "widget", "0.1.0");

    // Write a lockfile pointing at that package.
    let lock_path = home.join("wafer.lock");
    fs::write(
        &lock_path,
        r#"version = 1

[[package]]
name = "acme/widget"
version = "0.1.0"
source = "registry+https://example.test"
sha256 = "deadbeef"
"#,
    )
    .unwrap();

    // Redirect HOME so `default_cache_root()` resolves into our tempdir.
    let prev_home = env::var("HOME").ok();
    env::set_var("HOME", &home);

    let result = Wafer::builder()
        .disable_inventory()
        .lockfile(&lock_path)
        .build();

    // Restore HOME before any assertion that might panic.
    match prev_home {
        Some(h) => env::set_var("HOME", h),
        None => env::remove_var("HOME"),
    }

    let w = result.expect("builder with explicit lockfile should succeed");
    assert!(
        w.has_block("acme/widget"),
        "block 'acme/widget' should be registered after lockfile load"
    );
}

// ---------------------------------------------------------------------------
// Test 2: missing explicit lockfile path surfaces an error
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn missing_explicit_lockfile_errors() {
    let tmp = tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist.lock");

    let result = Wafer::builder()
        .disable_inventory()
        .lockfile(&missing)
        .build();
    let Err(err) = result else {
        panic!("builder with missing explicit lockfile must error");
    };

    let msg = err.to_string();
    assert!(
        msg.contains("does-not-exist.lock"),
        "error message should contain the missing filename, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: auto lockfile is silent when no wafer.lock exists in CWD
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn auto_lockfile_silent_when_missing() {
    // Ensure WAFER_LOCKFILE is not set so we fall through to the CWD default.
    let prev_wafer_lockfile = env::var("WAFER_LOCKFILE").ok();
    env::remove_var("WAFER_LOCKFILE");

    // Use a tempdir as HOME so dirs::home_dir() resolves somewhere clean and
    // `default_cache_root_or_err()` won't fail if somehow reached.
    let tmp = tempdir().unwrap();
    let prev_home = env::var("HOME").ok();
    env::set_var("HOME", tmp.path());

    let result = Wafer::builder().disable_inventory().build();

    match prev_home {
        Some(h) => env::set_var("HOME", h),
        None => env::remove_var("HOME"),
    }
    match prev_wafer_lockfile {
        Some(v) => env::set_var("WAFER_LOCKFILE", v),
        None => env::remove_var("WAFER_LOCKFILE"),
    }

    result.expect("auto-lockfile with no wafer.lock present must not error");
}

// ---------------------------------------------------------------------------
// Test 4: WAFER_LOCKFILE env var takes precedence over the default path
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn wafer_lockfile_env_var_takes_precedence() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().to_path_buf();

    // Seed a different block name to distinguish from test 1.
    seed_cache(&home, "acme", "envvar", "0.2.0");

    let lock_path: PathBuf = home.join("env.lock");
    fs::write(
        &lock_path,
        r#"version = 1

[[package]]
name = "acme/envvar"
version = "0.2.0"
source = "registry+https://example.test"
sha256 = "deadbeef"
"#,
    )
    .unwrap();

    let prev_home = env::var("HOME").ok();
    let prev_wafer_lockfile = env::var("WAFER_LOCKFILE").ok();

    env::set_var("HOME", &home);
    env::set_var("WAFER_LOCKFILE", lock_path.to_str().unwrap());

    // Use Auto lockfile source — WAFER_LOCKFILE env var should be picked up.
    let result = Wafer::builder().disable_inventory().build();

    match prev_home {
        Some(h) => env::set_var("HOME", h),
        None => env::remove_var("HOME"),
    }
    match prev_wafer_lockfile {
        Some(v) => env::set_var("WAFER_LOCKFILE", v),
        None => env::remove_var("WAFER_LOCKFILE"),
    }

    let w = result.expect("WAFER_LOCKFILE env var build should succeed");
    assert!(
        w.has_block("acme/envvar"),
        "block 'acme/envvar' should be registered when WAFER_LOCKFILE env var points at its lockfile"
    );
}
