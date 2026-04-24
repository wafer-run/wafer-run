//! Integration tests for `wafer install`'s full-feature semantics: full
//! install (single target with wafer.toml mutation), argument-less
//! install, --frozen, and flag-combination rejection.

use std::{fs, path::Path};

use flate2::{write::GzEncoder, Compression};
use tempfile::tempdir;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wafer")
}

fn make_tarball(version: &str) -> Vec<u8> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = tar::Builder::new(&mut gz);
        let manifest = format!(
            "[package]\norg = \"acme\"\nname = \"widget\"\nversion = \"{version}\"\nabi = 1\n"
        );
        let mut h = tar::Header::new_gnu();
        h.set_path("wafer.toml").unwrap();
        h.set_size(manifest.len() as u64);
        h.set_cksum();
        tar.append(&h, std::io::Cursor::new(manifest.as_bytes()))
            .unwrap();
        let wasm: &[u8] = b"\0asm\x01\x00\x00\x00";
        let mut h2 = tar::Header::new_gnu();
        h2.set_path("widget.wasm").unwrap();
        h2.set_size(wasm.len() as u64);
        h2.set_cksum();
        tar.append(&h2, std::io::Cursor::new(wasm)).unwrap();
        tar.finish().unwrap();
    }
    gz.finish().unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn setup_project(home: &Path, cwd: &Path, wafer_toml: &str) {
    fs::create_dir_all(home).unwrap();
    fs::create_dir_all(cwd).unwrap();
    fs::write(cwd.join("wafer.toml"), wafer_toml).unwrap();
}

async fn mount_version(server: &MockServer, version: &str, sha: &str, size: usize) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/registry/api/packages/acme/widget/{version}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "org_name": "acme", "pkg_name": "widget", "version": version,
            "abi": 1, "sha256": sha, "storage_key": "k", "size_bytes": size as i64,
            "license": null, "readme_md": null, "dependencies": null, "capabilities": null,
            "yanked": 0, "yanked_reason": null, "published_at": 0
        })))
        .mount(server)
        .await;
}

async fn mount_tarball(server: &MockServer, version: &str, bytes: Vec<u8>) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/registry/download/acme/widget/{version}.wafer"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
        .mount(server)
        .await;
}

#[tokio::test]
async fn full_install_mutates_wafer_toml_and_lockfile() {
    let server = MockServer::start().await;
    let tarball = make_tarball("0.3.1");
    let sha = sha256_hex(&tarball);
    mount_version(&server, "0.3.1", &sha, tarball.len()).await;
    mount_tarball(&server, "0.3.1", tarball.clone()).await;

    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(
        &home,
        &cwd,
        "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n",
    );

    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .env("WAFER_INSTALL_LOCK_TIMEOUT_SECS", "5")
        .current_dir(&cwd)
        .args(["install", "acme/widget@0.3.1", "--registry", &server.uri()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let toml = fs::read_to_string(cwd.join("wafer.toml")).unwrap();
    assert!(toml.contains("[dependencies]"), "{toml}");
    assert!(toml.contains("\"acme/widget\" = \"0.3.1\""), "{toml}");
    assert!(cwd.join("wafer.lock").is_file());
    assert!(home
        .join(".wafer/cache/acme/widget/0.3.1/wafer.toml")
        .is_file());
}

#[tokio::test]
async fn argumentless_install_reads_manifest_and_writes_lockfile() {
    let server = MockServer::start().await;
    let tarball = make_tarball("0.3.1");
    let sha = sha256_hex(&tarball);
    mount_version(&server, "0.3.1", &sha, tarball.len()).await;
    mount_tarball(&server, "0.3.1", tarball.clone()).await;

    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(
        &home,
        &cwd,
        "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n",
    );

    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .env("WAFER_INSTALL_LOCK_TIMEOUT_SECS", "5")
        .current_dir(&cwd)
        .args(["install", "--registry", &server.uri()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("installed acme/widget@0.3.1"), "{stdout}");
    assert!(cwd.join("wafer.lock").is_file());
}

#[tokio::test]
async fn argumentless_install_with_empty_deps_exits_clean() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(
        &home,
        &cwd,
        "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n",
    );
    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .current_dir(&cwd)
        .args(["install", "--registry", "http://127.0.0.1:1"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no dependencies"), "{stdout}");
}

#[tokio::test]
async fn argumentless_install_prunes_orphan_lockfile_entries() {
    let server = MockServer::start().await;
    let tarball = make_tarball("0.3.1");
    let sha = sha256_hex(&tarball);
    mount_version(&server, "0.3.1", &sha, tarball.len()).await;
    mount_tarball(&server, "0.3.1", tarball.clone()).await;

    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(
        &home,
        &cwd,
        "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n",
    );
    fs::write(
        cwd.join("wafer.lock"),
        "version = 1\n\n[[package]]\nname = \"old/removed\"\nversion = \"0.1.0\"\nsha256 = \"aa\"\nsource = \"registry+http://x\"\n",
    )
    .unwrap();

    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .env("WAFER_INSTALL_LOCK_TIMEOUT_SECS", "5")
        .current_dir(&cwd)
        .args(["install", "--registry", &server.uri()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lf = fs::read_to_string(cwd.join("wafer.lock")).unwrap();
    assert!(lf.contains("acme/widget"), "{lf}");
    assert!(!lf.contains("old/removed"), "{lf}");
}

#[tokio::test]
async fn frozen_errors_on_missing_lockfile() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(
        &home,
        &cwd,
        "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n",
    );
    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .current_dir(&cwd)
        .args(["install", "--frozen", "--registry", "http://127.0.0.1:1"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("wafer.lock not found"), "{stderr}");
}

#[tokio::test]
async fn frozen_errors_on_drift_with_hint() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(
        &home,
        &cwd,
        "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n",
    );
    fs::write(
        cwd.join("wafer.lock"),
        "version = 1\n\n[[package]]\nname = \"acme/widget\"\nversion = \"0.2.0\"\nsha256 = \"aa\"\nsource = \"registry+http://x\"\n",
    )
    .unwrap();
    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .current_dir(&cwd)
        .args(["install", "--frozen", "--registry", "http://127.0.0.1:1"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("out of sync"), "{stderr}");
    assert!(
        stderr.contains("hint: run 'wafer install' without --frozen"),
        "{stderr}"
    );
}

#[tokio::test]
async fn frozen_happy_path_uses_cache_and_doesnt_rewrite_lockfile() {
    let server = MockServer::start().await;
    let tarball = make_tarball("0.3.1");
    let sha = sha256_hex(&tarball);
    mount_version(&server, "0.3.1", &sha, tarball.len()).await;
    mount_tarball(&server, "0.3.1", tarball.clone()).await;

    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(
        &home,
        &cwd,
        "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n",
    );
    let lock_body = format!(
        "version = 1\n\n[[package]]\nname = \"acme/widget\"\nversion = \"0.3.1\"\nsha256 = \"{sha}\"\nsource = \"registry+{uri}\"\n",
        uri = server.uri(),
    );
    fs::write(cwd.join("wafer.lock"), &lock_body).unwrap();

    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .env("WAFER_INSTALL_LOCK_TIMEOUT_SECS", "5")
        .current_dir(&cwd)
        .args(["install", "--frozen", "--registry", &server.uri()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lock_after = fs::read_to_string(cwd.join("wafer.lock")).unwrap();
    assert_eq!(lock_after, lock_body, "frozen mutated the lockfile");
}

#[tokio::test]
async fn flag_combinations_rejected() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(
        &home,
        &cwd,
        "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n",
    );

    // --cache-only + --frozen → rejected.
    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .current_dir(&cwd)
        .args([
            "install",
            "acme/widget",
            "--cache-only",
            "--frozen",
            "--registry",
            "http://127.0.0.1:1",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("mutually exclusive"), "{stderr}");

    // target + --frozen → rejected.
    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .current_dir(&cwd)
        .args([
            "install",
            "acme/widget",
            "--frozen",
            "--registry",
            "http://127.0.0.1:1",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--frozen does not accept a target"),
        "{stderr}"
    );

    // --cache-only without target → rejected.
    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .current_dir(&cwd)
        .args([
            "install",
            "--cache-only",
            "--registry",
            "http://127.0.0.1:1",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("requires an"), "{stderr}");
}
