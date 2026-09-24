//! End-to-end test for `wafer login`: spawns the binary, pipes a paste-token
//! over stdin, and asserts the registry `/me` round-trip persists credentials
//! to a temp `XDG_CONFIG_HOME`.

use std::{
    io::Write,
    process::{Command, Stdio},
};

use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

/// A registry that exchanges any login code for `token` and answers `/me`
/// only when that token is presented.
async fn registry_issuing(token: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/registry/api/cli-login/exchange"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "token": token,
            "user": { "email": "alice@example.com" }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/registry/api/me"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "email": "alice@example.com",
            "is_admin": false
        })))
        .mount(&server)
        .await;
    server
}

/// Run `wafer <args> --registry <registry>` with `HOME` at `home`, feeding
/// `stdin`, and return the output.
fn wafer(
    home: &std::path::Path,
    args: &[&str],
    registry: &str,
    stdin: &[u8],
) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_wafer"))
        .args(args)
        .arg("--registry")
        .arg(registry)
        .env("HOME", home)
        .env_remove("WAFER_REGISTRY")
        .env("BROWSER", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wafer");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin)
        .expect("write stdin");
    child.wait_with_output().expect("wait wafer")
}

fn assert_success(what: &str, out: &std::process::Output) {
    assert!(
        out.status.success(),
        "{what} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Logging in to a second registry keeps the first registry's token: both
/// `whoami`s present the token their own registry issued.
#[tokio::test]
async fn login_to_a_second_registry_keeps_the_first_token() {
    let first = registry_issuing("wafer_pat_first").await;
    let second = registry_issuing("wafer_pat_second").await;
    let tmp = tempfile::tempdir().expect("tempdir");

    assert_success(
        "login first",
        &wafer(tmp.path(), &["login"], &first.uri(), b"code\n"),
    );
    assert_success(
        "login second",
        &wafer(tmp.path(), &["login"], &second.uri(), b"code\n"),
    );

    assert_success(
        "whoami first",
        &wafer(tmp.path(), &["whoami"], &first.uri(), b""),
    );
    assert_success(
        "whoami second",
        &wafer(tmp.path(), &["whoami"], &second.uri(), b""),
    );
}

/// A credentials file in the legacy layout (`[default]` entry) keeps
/// authenticating, and the next login rewrites it in the current layout
/// without losing the legacy token.
#[tokio::test]
async fn legacy_credentials_file_keeps_working_and_is_rewritten() {
    let legacy = registry_issuing("wafer_pat_legacy").await;
    let other = registry_issuing("wafer_pat_other").await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cred_path = tmp.path().join(".wafer").join("credentials.toml");
    std::fs::create_dir_all(cred_path.parent().unwrap()).unwrap();
    std::fs::write(
        &cred_path,
        format!(
            "[default]\nregistry = \"{}\"\ntoken = \"wafer_pat_legacy\"\n",
            legacy.uri()
        ),
    )
    .unwrap();

    assert_success(
        "whoami legacy",
        &wafer(tmp.path(), &["whoami"], &legacy.uri(), b""),
    );

    assert_success(
        "login other",
        &wafer(tmp.path(), &["login"], &other.uri(), b"code\n"),
    );
    let contents = std::fs::read_to_string(&cred_path).unwrap();
    assert!(
        !contents.contains("[default]"),
        "legacy layout must not be written back: {contents}"
    );

    assert_success(
        "whoami legacy after login",
        &wafer(tmp.path(), &["whoami"], &legacy.uri(), b""),
    );
    assert_success(
        "whoami other",
        &wafer(tmp.path(), &["whoami"], &other.uri(), b""),
    );
}

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
