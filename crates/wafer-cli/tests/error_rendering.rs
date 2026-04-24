//! End-to-end: run the `wafer` binary against a wiremock server and assert
//! that the rendered stderr contains the envelope message + the expected
//! hint for known error codes.

use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wafer")
}

fn write_credentials(home: &std::path::Path, registry_url: &str, token: &str) {
    std::fs::create_dir_all(home.join(".wafer")).unwrap();
    let toml = format!("[default]\nregistry = \"{registry_url}\"\ntoken = \"{token}\"\n");
    std::fs::write(home.join(".wafer/credentials.toml"), toml).unwrap();
}

fn setup_project(tmp: &std::path::Path) -> std::path::PathBuf {
    std::fs::write(
        tmp.join("wafer.toml"),
        "[package]\norg = \"acme\"\nname = \"widget\"\nversion = \"0.1.0\"\nabi = 1\n",
    )
    .unwrap();
    let tb_path = tmp.join("tb.wafer");
    std::fs::write(&tb_path, b"fake-tarball").unwrap();
    tb_path
}

#[tokio::test]
async fn publish_409_version_exists_renders_envelope_and_hint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/registry/api/publish"))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
            "error": "version-exists",
            "message": "acme/widget@0.1.0 already published"
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    write_credentials(tmp.path(), &server.uri(), "wafer_pat_xyz");
    let tb_path = setup_project(tmp.path());

    let out = std::process::Command::new(bin())
        .env("HOME", tmp.path())
        .env_remove("WAFER_REGISTRY")
        .current_dir(tmp.path())
        .args([
            "publish",
            "--file",
            tb_path.to_str().unwrap(),
            "--registry",
            &server.uri(),
        ])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("publish failed (409 Conflict)"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("acme/widget@0.1.0 already published"),
        "envelope message missing: {stderr}"
    );
    assert!(
        stderr.contains("hint: bump 'version' in wafer.toml"),
        "hint missing: {stderr}"
    );
    // The raw JSON must not leak through.
    assert!(
        !stderr.contains("\"error\":\"version-exists\""),
        "raw JSON leaked: {stderr}"
    );
}

#[tokio::test]
async fn publish_401_renders_login_hint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/registry/api/publish"))
        .respond_with(ResponseTemplate::new(401).set_body_string(""))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    write_credentials(tmp.path(), &server.uri(), "wafer_pat_xyz");
    let tb_path = setup_project(tmp.path());

    let out = std::process::Command::new(bin())
        .env("HOME", tmp.path())
        .env_remove("WAFER_REGISTRY")
        .current_dir(tmp.path())
        .args([
            "publish",
            "--file",
            tb_path.to_str().unwrap(),
            "--registry",
            &server.uri(),
        ])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("publish failed (401 Unauthorized)"),
        "{stderr}"
    );
    assert!(stderr.contains("hint: run 'wafer login'"), "{stderr}");
}
