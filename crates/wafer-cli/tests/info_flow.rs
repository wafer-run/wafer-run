//! End-to-end: `wafer info` against a wiremock registry.

use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wafer")
}

fn package_detail_body() -> serde_json::Value {
    // Versions ordered newest→oldest by published_at. 2026-04-12 is the newest.
    serde_json::json!({
        "org": "acme",
        "name": "widget",
        "summary": "A widget block for the acme toolkit.",
        "versions": [
            {"version":"0.3.1","abi":1,"sha256":"a","size_bytes":120300,"license":"Apache-2.0","yanked":0,"published_at":1775952000},
            {"version":"0.3.0","abi":1,"sha256":"b","size_bytes":118000,"license":"Apache-2.0","yanked":0,"published_at":1775865600},
            {"version":"0.2.9","abi":1,"sha256":"c","size_bytes":117000,"license":"Apache-2.0","yanked":1,"published_at":1775779200},
            {"version":"0.2.8","abi":1,"sha256":"d","size_bytes":116000,"license":"Apache-2.0","yanked":0,"published_at":1775692800},
            {"version":"0.2.7","abi":1,"sha256":"e","size_bytes":115000,"license":"Apache-2.0","yanked":0,"published_at":1775606400},
            {"version":"0.2.6","abi":1,"sha256":"f","size_bytes":114000,"license":"Apache-2.0","yanked":0,"published_at":1775520000},
            {"version":"0.2.5","abi":1,"sha256":"g","size_bytes":113000,"license":"Apache-2.0","yanked":0,"published_at":1775433600}
        ]
    })
}

fn version_detail_body(yanked: bool) -> serde_json::Value {
    serde_json::json!({
        "org_name": "acme",
        "pkg_name": "widget",
        "version": "0.3.1",
        "abi": 1,
        "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "storage_key": "storage/acme/widget/0.3.1.wafer",
        "size_bytes": 120300,
        "license": "Apache-2.0",
        "readme_md": null,
        "dependencies": null,
        "capabilities": null,
        "yanked": if yanked { 1 } else { 0 },
        "yanked_reason": if yanked { serde_json::Value::String("security".into()) } else { serde_json::Value::Null },
        "published_at": 1775952000
    })
}

#[tokio::test]
async fn info_package_form_shows_summary_latest_and_top5() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget"))
        .respond_with(ResponseTemplate::new(200).set_body_json(package_detail_body()))
        .mount(&server)
        .await;

    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args(["info", "acme/widget", "--registry", &server.uri()])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("acme/widget\n"), "{stdout}");
    assert!(
        stdout.contains("summary:  A widget block for the acme toolkit."),
        "{stdout}"
    );
    assert!(stdout.contains("latest:   0.3.1"), "{stdout}");
    assert!(stdout.contains("versions: 7  (1 yanked)"), "{stdout}");

    // 5 versions rendered, not 7 (the two oldest should be hidden).
    assert!(stdout.contains("0.3.1"), "{stdout}");
    assert!(stdout.contains("0.2.7"), "{stdout}");
    assert!(!stdout.contains("0.2.6"), "{stdout}");
    assert!(!stdout.contains("0.2.5"), "{stdout}");
}

#[tokio::test]
async fn info_package_all_shows_every_version() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget"))
        .respond_with(ResponseTemplate::new(200).set_body_json(package_detail_body()))
        .mount(&server)
        .await;

    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args(["info", "acme/widget", "--all", "--registry", &server.uri()])
        .output()
        .unwrap();

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("0.2.5"), "{stdout}");
}

#[tokio::test]
async fn info_version_form_renders_version_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget/0.3.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(version_detail_body(false)))
        .mount(&server)
        .await;

    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args(["info", "acme/widget@0.3.1", "--registry", &server.uri()])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("YANKED"), "{stdout}");
    assert!(stdout.contains("acme/widget@0.3.1"), "{stdout}");
    assert!(stdout.contains("published:   2026-04-12"), "{stdout}");
    assert!(stdout.contains("abi:         1"), "{stdout}");
    assert!(stdout.contains("size:        117.5 KiB"), "{stdout}");
    assert!(
        stdout.contains(
            "sha256:      e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        ),
        "{stdout}"
    );
    assert!(stdout.contains("license:     Apache-2.0"), "{stdout}");
    let expected_dl = format!(
        "download:    {}/registry/download/acme/widget/0.3.1.wafer",
        server.uri().trim_end_matches('/')
    );
    assert!(stdout.contains(&expected_dl), "{stdout}");
    assert!(
        stdout.contains("install:     wafer install acme/widget@0.3.1"),
        "{stdout}"
    );
}

#[tokio::test]
async fn info_yanked_version_shows_banner() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget/0.3.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(version_detail_body(true)))
        .mount(&server)
        .await;

    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args(["info", "acme/widget@0.3.1", "--registry", &server.uri()])
        .output()
        .unwrap();

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("⚠ THIS VERSION IS YANKED"), "{stdout}");
}

#[tokio::test]
async fn info_package_404_renders_search_hint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": "not-found",
            "message": "Package acme/widget not found"
        })))
        .mount(&server)
        .await;

    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args(["info", "acme/widget", "--registry", &server.uri()])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("info failed (404"), "{stderr}");
    assert!(stderr.contains("Package acme/widget not found"), "{stderr}");
    assert!(stderr.contains("hint: 'wafer search' can help"), "{stderr}");
}

#[tokio::test]
async fn info_version_json_emits_version_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget/0.3.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(version_detail_body(false)))
        .mount(&server)
        .await;

    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args([
            "info",
            "acme/widget@0.3.1",
            "--json",
            "--registry",
            &server.uri(),
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["version"]["version"], "0.3.1");
    assert_eq!(parsed["version"]["org_name"], "acme");
}

#[tokio::test]
async fn info_package_json_emits_package_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/registry/api/packages/acme/widget"))
        .respond_with(ResponseTemplate::new(200).set_body_json(package_detail_body()))
        .mount(&server)
        .await;

    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args(["info", "acme/widget", "--json", "--registry", &server.uri()])
        .output()
        .unwrap();

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["package"]["org"], "acme");
    assert_eq!(parsed["package"]["name"], "widget");
    assert_eq!(parsed["package"]["versions"].as_array().unwrap().len(), 7);
}

#[tokio::test]
async fn info_malformed_target_fails_locally() {
    // No server; must reject before any network call.
    let out = std::process::Command::new(bin())
        .env_remove("WAFER_REGISTRY")
        .args(["info", "just-a-name", "--registry", "http://127.0.0.1:1"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("target must be org/block"), "{stderr}");
}
