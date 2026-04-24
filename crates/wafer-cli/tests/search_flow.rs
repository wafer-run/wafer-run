//! End-to-end: `wafer search` against a wiremock registry. Asserts table
//! output, --json output, empty-result handling, and local empty-query
//! rejection.

use wiremock::{
    matchers::{method, path, query_param},
    Mock, MockServer, ResponseTemplate,
};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wafer")
}

#[tokio::test]
async fn search_happy_renders_table() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/search"))
        .and(query_param("q", "widget"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "packages": [
                {"org":"acme","name":"widget","summary":"A widget block.","latest":"0.3.1"},
                {"org":"acme","name":"widget-extras","summary":"Companion block.","latest":"0.1.0"}
            ],
            "total": 2,
            "query": "widget",
            "page": 1,
            "page_size": 20
        })))
        .mount(&server)
        .await;

    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args(["search", "widget", "--registry", &server.uri()])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ORG/BLOCK"), "{stdout}");
    assert!(stdout.contains("acme/widget"), "{stdout}");
    assert!(stdout.contains("0.3.1"), "{stdout}");
    assert!(stdout.contains("A widget block."), "{stdout}");
}

#[tokio::test]
async fn search_empty_results_prints_no_matches() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "packages": [],
            "total": 0,
            "query": "nope",
            "page": 1,
            "page_size": 20
        })))
        .mount(&server)
        .await;

    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args(["search", "nope", "--registry", &server.uri()])
        .output()
        .unwrap();

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no matches for 'nope'"), "{stdout}");
}

#[tokio::test]
async fn search_json_emits_packages_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "packages": [
                {"org":"acme","name":"widget","summary":null,"latest":"0.3.1"}
            ],
            "total": 1,
            "query": "widget",
            "page": 1,
            "page_size": 20
        })))
        .mount(&server)
        .await;

    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args(["search", "widget", "--json", "--registry", &server.uri()])
        .output()
        .unwrap();

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let pkgs = parsed["packages"].as_array().expect("packages array");
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0]["org"], "acme");
    assert_eq!(pkgs[0]["name"], "widget");
    assert_eq!(pkgs[0]["latest"], "0.3.1");
}

#[tokio::test]
async fn search_empty_query_fails_locally_without_network() {
    // No MockServer needed — the CLI must reject before any network call.
    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args(["search", "", "--registry", "http://127.0.0.1:1"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("search requires a non-empty query"),
        "{stderr}"
    );
}

#[tokio::test]
async fn search_server_500_surfaces_registry_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/search"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args(["search", "x", "--registry", &server.uri()])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("search failed (500"), "{stderr}");
}
