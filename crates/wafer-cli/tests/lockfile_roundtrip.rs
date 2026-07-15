//! End-to-end: `wafer install --cache-only` with a **populated cache**
//! + a **lockfile entry that matches**: no network, just reports cached.
//!
//! Other integration tests in install_flow.rs cover the full flow with
//! wiremock; this file exists to prove the cache-hit fast path is free
//! of network calls even when the registry is unreachable.

use std::fs;

use tempfile::tempdir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wafer")
}

#[test]
fn cache_populated_with_matching_lockfile_skips_network() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();

    // Seed wafer.toml (existence check — step 1 of install).
    fs::write(
        cwd.join("wafer.toml"),
        "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n",
    )
    .unwrap();

    // Seed cache contents.
    let version_dir = home.join(".wafer/cache/acme/widget/0.3.1");
    fs::create_dir_all(&version_dir).unwrap();
    fs::write(version_dir.join("wafer.toml"), b"[package]\n").unwrap();
    fs::write(version_dir.join("widget.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();

    // Seed matching lockfile entry.
    let sha = "deadbeef".repeat(8); // 64 hex chars; arbitrary for a cache-hit test.
                                    // The cache-hit fast path reads the entry without loading the wasm, so
                                    // wasm_sha256 only needs to satisfy the v2 schema here.
    let lockfile = format!(
        "version = 2\n\n[[package]]\nname = \"acme/widget\"\nversion = \"0.3.1\"\nsha256 = \"{sha}\"\nwasm_sha256 = \"{sha}\"\nsource = \"registry+http://127.0.0.1:1\"\n"
    );
    fs::write(cwd.join("wafer.lock"), lockfile).unwrap();

    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .current_dir(&cwd)
        .args([
            "install",
            "acme/widget@0.3.1",
            "--cache-only",
            // Unreachable registry — if network was touched, this fails.
            "--registry",
            "http://127.0.0.1:1",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("installed acme/widget@0.3.1"), "{stdout}");
    assert!(stdout.contains("(cached)"), "{stdout}");
}
