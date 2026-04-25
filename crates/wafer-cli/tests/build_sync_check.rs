//! Verify `wafer build`'s sync check errors on drift without requiring
//! any network or toolchain.
//!
//! `wafer build` fails in our tempdirs because no manifest.json exists,
//! but that's a different error — so we can distinguish "sync check
//! fired" from "sync check didn't fire" by looking at stderr.

use std::fs;

use tempfile::tempdir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wafer")
}

fn seed(cwd: &std::path::Path, toml: &str, lock: &str) {
    fs::create_dir_all(cwd).unwrap();
    fs::write(cwd.join("wafer.toml"), toml).unwrap();
    fs::write(cwd.join("wafer.lock"), lock).unwrap();
}

#[test]
fn build_drift_errors_with_hint() {
    let tmp = tempdir().unwrap();
    let cwd = tmp.path().join("proj");
    seed(
        &cwd,
        "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n",
        "version = 1\n\n[[package]]\nname = \"acme/widget\"\nversion = \"0.2.0\"\nsha256 = \"aa\"\nsource = \"registry+http://x\"\n",
    );
    // Also need manifest.json so that Manifest::load succeeds (otherwise
    // we'd bail before the sync check). Seed a minimal one.
    fs::write(
        cwd.join("manifest.json"),
        "{\"name\":\"me/me\",\"version\":\"0.0.1\",\"interface\":\"handler@v1\",\"summary\":\"x\"}",
    )
    .unwrap();
    let out = std::process::Command::new(bin())
        .current_dir(&cwd)
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("out of sync"), "{stderr}");
    assert!(stderr.contains("acme/widget"), "{stderr}");
    assert!(stderr.contains("hint: run 'wafer install'"), "{stderr}");
}

#[test]
fn build_in_sync_proceeds_past_sync_check() {
    let tmp = tempdir().unwrap();
    let cwd = tmp.path().join("proj");
    seed(
        &cwd,
        "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n",
        "version = 1\n\n[[package]]\nname = \"acme/widget\"\nversion = \"0.3.1\"\nsha256 = \"aa\"\nsource = \"registry+http://x\"\n",
    );
    fs::write(
        cwd.join("manifest.json"),
        "{\"name\":\"me/me\",\"version\":\"0.0.1\",\"interface\":\"handler@v1\",\"summary\":\"x\"}",
    )
    .unwrap();
    let out = std::process::Command::new(bin())
        .current_dir(&cwd)
        .arg("build")
        .output()
        .unwrap();
    // Build will fail for a different reason (no toolchain / no source).
    // Assert the failure is NOT a sync-check error.
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("out of sync"), "{stderr}");
}

#[test]
fn build_without_wafer_toml_skips_sync_check() {
    let tmp = tempdir().unwrap();
    let cwd = tmp.path().join("proj");
    fs::create_dir_all(&cwd).unwrap();
    // No wafer.toml — sync check must be silent even if a stray wafer.lock exists.
    fs::write(
        cwd.join("wafer.lock"),
        "version = 1\n\n[[package]]\nname = \"a/b\"\nversion = \"1.0.0\"\nsha256 = \"a\"\nsource = \"registry+http://x\"\n",
    ).unwrap();
    fs::write(
        cwd.join("manifest.json"),
        "{\"name\":\"me/me\",\"version\":\"0.0.1\",\"interface\":\"handler@v1\",\"summary\":\"x\"}",
    )
    .unwrap();
    let out = std::process::Command::new(bin())
        .current_dir(&cwd)
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("out of sync"), "{stderr}");
}
