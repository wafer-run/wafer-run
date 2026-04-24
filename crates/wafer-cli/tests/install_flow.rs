//! End-to-end: `wafer install --cache-only` against a wiremock registry.
//!
//! Coverage:
//! - Happy explicit @ver install.
//! - Happy no-@ver install (latest, skip yanked).
//! - All versions yanked → error.
//! - sha256 mismatch → bails, no partial extraction, no lockfile write.
//! - 404 on version detail → error surfaces cleanly.
//! - Explicit yanked version → succeeds with stderr warning.

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

fn make_tarball(name_wasm: &str) -> Vec<u8> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = tar::Builder::new(&mut gz);
        let manifest =
            b"[package]\norg = \"acme\"\nname = \"widget\"\nversion = \"0.3.1\"\nabi = 1\n";
        let mut h = tar::Header::new_gnu();
        h.set_path("wafer.toml").unwrap();
        h.set_size(manifest.len() as u64);
        h.set_cksum();
        tar.append(&h, std::io::Cursor::new(manifest)).unwrap();

        let wasm: &[u8] = b"\0asm\x01\x00\x00\x00";
        let mut h2 = tar::Header::new_gnu();
        h2.set_path(name_wasm).unwrap();
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

fn setup_project(home: &Path, cwd: &Path) {
    fs::create_dir_all(home).unwrap();
    fs::create_dir_all(cwd).unwrap();
    fs::write(
        cwd.join("wafer.toml"),
        "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n",
    )
    .unwrap();
}

#[tokio::test]
async fn install_happy_explicit_version() {
    let server = MockServer::start().await;
    let tarball = make_tarball("widget.wasm");
    let sha = sha256_hex(&tarball);

    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget/0.3.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "org_name": "acme", "pkg_name": "widget", "version": "0.3.1",
            "abi": 1, "sha256": sha, "storage_key": "k", "size_bytes": tarball.len() as i64,
            "license": null, "readme_md": null, "dependencies": null, "capabilities": null,
            "yanked": 0, "yanked_reason": null, "published_at": 0
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/registry/download/acme/widget/0.3.1.wafer"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tarball.clone()))
        .mount(&server)
        .await;

    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(&home, &cwd);

    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .env("WAFER_INSTALL_LOCK_TIMEOUT_SECS", "5")
        .current_dir(&cwd)
        .args([
            "install",
            "acme/widget@0.3.1",
            "--cache-only",
            "--registry",
            &server.uri(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("installed acme/widget@0.3.1"), "{stdout}");
    assert!(!stdout.contains("(cached)"), "{stdout}");

    // Cache was populated.
    assert!(home
        .join(".wafer/cache/acme/widget/0.3.1/wafer.toml")
        .is_file());
    assert!(home
        .join(".wafer/cache/acme/widget/0.3.1/widget.wasm")
        .is_file());

    // Lockfile was written.
    let lf = fs::read_to_string(cwd.join("wafer.lock")).unwrap();
    assert!(lf.contains("[[package]]"), "{lf}");
    assert!(lf.contains("\"acme/widget\""), "{lf}");
    assert!(lf.contains(&sha), "{lf}");
}

#[tokio::test]
async fn install_no_version_picks_latest_non_yanked() {
    let server = MockServer::start().await;
    let tarball = make_tarball("widget.wasm");
    let sha = sha256_hex(&tarball);

    // Package detail: two versions; newest (0.4.0) is yanked.
    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "org": "acme", "name": "widget", "summary": null,
            "versions": [
                {"version":"0.4.0","abi":1,"sha256":"z","size_bytes":1,"license":null,"yanked":1,"published_at":9},
                {"version":"0.3.1","abi":1,"sha256": sha, "size_bytes":42,"license":null,"yanked":0,"published_at":3}
            ]
        })))
        .mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/registry/download/acme/widget/0.3.1.wafer"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tarball))
        .mount(&server)
        .await;

    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(&home, &cwd);

    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .env("WAFER_INSTALL_LOCK_TIMEOUT_SECS", "5")
        .current_dir(&cwd)
        .args([
            "install",
            "acme/widget",
            "--cache-only",
            "--registry",
            &server.uri(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(home.join(".wafer/cache/acme/widget/0.3.1").is_dir());
}

#[tokio::test]
async fn install_all_yanked_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "org": "acme", "name": "widget", "summary": null,
            "versions": [
                {"version":"0.1.0","abi":1,"sha256":"a","size_bytes":1,"license":null,"yanked":1,"published_at":0}
            ]
        })))
        .mount(&server).await;

    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(&home, &cwd);

    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .env("WAFER_INSTALL_LOCK_TIMEOUT_SECS", "5")
        .current_dir(&cwd)
        .args([
            "install",
            "acme/widget",
            "--cache-only",
            "--registry",
            &server.uri(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no non-yanked versions of acme/widget"),
        "{stderr}"
    );
}

#[tokio::test]
async fn install_sha_mismatch_bails_cleanly() {
    let server = MockServer::start().await;
    let tarball = make_tarball("widget.wasm");
    // Publish a sha that doesn't match the tarball.
    let bogus_sha = "0".repeat(64);
    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget/0.3.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "org_name":"acme","pkg_name":"widget","version":"0.3.1",
            "abi":1,"sha256":bogus_sha,"storage_key":"k","size_bytes": tarball.len() as i64,
            "license":null,"readme_md":null,"dependencies":null,"capabilities":null,
            "yanked":0,"yanked_reason":null,"published_at":0
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/registry/download/acme/widget/0.3.1.wafer"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tarball))
        .mount(&server)
        .await;

    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(&home, &cwd);

    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .env("WAFER_INSTALL_LOCK_TIMEOUT_SECS", "5")
        .current_dir(&cwd)
        .args([
            "install",
            "acme/widget@0.3.1",
            "--cache-only",
            "--registry",
            &server.uri(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("integrity check failed"), "{stderr}");

    // No partial extraction.
    assert!(!home.join(".wafer/cache/acme/widget/0.3.1").exists());
    // No lockfile written.
    assert!(!cwd.join("wafer.lock").exists());
}

#[tokio::test]
async fn install_404_on_version_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget/0.3.1"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": "not-found",
            "message": "acme/widget@0.3.1 not found"
        })))
        .mount(&server)
        .await;

    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(&home, &cwd);

    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .env("WAFER_INSTALL_LOCK_TIMEOUT_SECS", "5")
        .current_dir(&cwd)
        .args([
            "install",
            "acme/widget@0.3.1",
            "--cache-only",
            "--registry",
            &server.uri(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    // `download_tarball` uses op="install", but the version-fetch uses op="info".
    // Either surfaces the 404 envelope's message.
    assert!(stderr.contains("acme/widget@0.3.1 not found"), "{stderr}");
}

#[tokio::test]
async fn install_explicit_yanked_succeeds_with_warning() {
    let server = MockServer::start().await;
    let tarball = make_tarball("widget.wasm");
    let sha = sha256_hex(&tarball);
    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget/0.3.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "org_name":"acme","pkg_name":"widget","version":"0.3.1",
            "abi":1,"sha256": sha, "storage_key":"k","size_bytes": tarball.len() as i64,
            "license":null,"readme_md":null,"dependencies":null,"capabilities":null,
            "yanked": 1, "yanked_reason":"bad","published_at":0
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/registry/download/acme/widget/0.3.1.wafer"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tarball))
        .mount(&server)
        .await;

    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    setup_project(&home, &cwd);

    let out = std::process::Command::new(bin())
        .env("HOME", &home)
        .env_remove("WAFER_REGISTRY")
        .env("WAFER_INSTALL_LOCK_TIMEOUT_SECS", "5")
        .current_dir(&cwd)
        .args([
            "install",
            "acme/widget@0.3.1",
            "--cache-only",
            "--registry",
            &server.uri(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("was yanked"), "{stderr}");
}
