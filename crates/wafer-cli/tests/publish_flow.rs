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

#[tokio::test]
async fn publish_happy() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/registry/api/publish"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "package": "acme/widget",
            "version": "0.1.0",
            "download_url": "/registry/download/acme/widget/0.1.0.wafer",
            "sha256": "abc"
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    write_credentials(tmp.path(), &server.uri(), "wafer_pat_xyz");

    std::fs::write(
        tmp.path().join("wafer.toml"),
        "[package]\norg = \"acme\"\nname = \"widget\"\nversion = \"0.1.0\"\nabi = 1\n",
    )
    .unwrap();
    let tb_path = tmp.path().join("tb.wafer");
    std::fs::write(&tb_path, b"fake-tarball").unwrap();

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

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Published"), "stdout: {stdout}");
}

#[tokio::test]
async fn publish_401_surfaces_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/registry/api/publish"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    write_credentials(tmp.path(), &server.uri(), "wafer_pat_invalid");

    std::fs::write(
        tmp.path().join("wafer.toml"),
        "[package]\norg = \"acme\"\nname = \"widget\"\nversion = \"0.1.0\"\nabi = 1\n",
    )
    .unwrap();
    let tb_path = tmp.path().join("tb.wafer");
    std::fs::write(&tb_path, b"fake-tarball").unwrap();

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
        stderr.contains("401"),
        "stderr should contain 401 status code: {stderr}"
    );
}

#[tokio::test]
async fn publish_missing_token_errors() {
    let tmp = tempfile::tempdir().unwrap();

    std::fs::write(
        tmp.path().join("wafer.toml"),
        "[package]\norg = \"acme\"\nname = \"widget\"\nversion = \"0.1.0\"\nabi = 1\n",
    )
    .unwrap();
    let tb_path = tmp.path().join("tb.wafer");
    std::fs::write(&tb_path, b"fake-tarball").unwrap();

    let out = std::process::Command::new(bin())
        .env("HOME", tmp.path())
        .env_remove("WAFER_REGISTRY")
        .current_dir(tmp.path())
        .args([
            "publish",
            "--file",
            tb_path.to_str().unwrap(),
            "--registry",
            "https://wafer.run",
        ])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("login") || stderr.contains("token"),
        "stderr should mention login or token: {stderr}"
    );
}
