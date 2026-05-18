//! End-to-end test for `wafer login`: spawns the binary, pipes a paste-token
//! over stdin, and asserts the registry `/me` round-trip persists credentials
//! to a temp `XDG_CONFIG_HOME`.

use std::{
    io::Write,
    process::{Command, Stdio},
};

use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn login_persists_token_to_credentials_toml() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/registry/api/cli-login/exchange"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "token": "wafer_pat_xyz",
            "user": { "email": "alice@example.com" }
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_home = tmp.path();

    let bin = env!("CARGO_BIN_EXE_wafer");
    let mut child = Command::new(bin)
        .arg("login")
        .arg("--registry")
        .arg(server.uri())
        .env("HOME", fake_home)
        // Prevent opening a browser or hitting real env overrides during tests.
        .env_remove("WAFER_REGISTRY")
        .env("BROWSER", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wafer login");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(b"deadbeef\n").expect("write code");
    }

    let out = child.wait_with_output().expect("wait wafer");
    assert!(
        out.status.success(),
        "wafer login failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let cred_path = fake_home.join(".wafer").join("credentials.toml");
    let contents = std::fs::read_to_string(&cred_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", cred_path.display()));
    assert!(
        contents.contains("wafer_pat_xyz"),
        "credentials missing token: {contents}"
    );
    assert!(
        contents.contains(&server.uri()),
        "credentials missing registry url: {contents}"
    );
}
