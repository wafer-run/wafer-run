//! SEC-09 e2e: SSRF filtering on registry downloads.
//!
//! The registry client refuses to fetch from private/internal addresses in
//! default builds: the composed manifest URL is pre-checked with
//! `wafer_net_security::is_blocked_url` and DNS results are filtered by
//! `SsrfFilteringResolver` (rebinding). The `allow-private-network` build
//! feature is the compile-time escape hatch for local registries — under it
//! the same wiremock registry serves a manifest + wasm artifact end-to-end.
//!
//! Env-var discipline: `WAFER_RUN_REGISTRY_BASE_URL` is process-global, so
//! each build flavor keeps ALL its phases inside one `#[tokio::test]` (the
//! `wasm_pooling_env_kill_switch` precedent) and restores the var on exit.

#![cfg(feature = "wasm")]

use serde_json::json;
use wafer_run::{StaticConfigSource, Wafer, REGISTRY_BASE_URL_KEY};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn new_wafer() -> Wafer {
    let cfg: std::sync::Arc<dyn wafer_run::ConfigSource> =
        std::sync::Arc::new(StaticConfigSource::default());
    Wafer::new(cfg).expect("Wafer::new")
}

/// Serve a well-formed registry for `acme/widget@1.0.0` whose wasm artifact
/// is the echo-block test fixture.
async fn mock_registry() -> MockServer {
    let server = MockServer::start().await;
    let manifest = json!({
        "name": "acme/widget",
        "latest": "1.0.0",
        "versions": {
            "1.0.0": {
                "abi": wafer_run::ABI_VERSION,
                "wasm_url": format!("{}/acme/widget/1.0.0/block.wasm", server.uri()),
                "flow_url": null,
            }
        }
    });
    Mock::given(method("GET"))
        .and(path("/acme/widget/manifest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&manifest))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/acme/widget/1.0.0/block.wasm"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(include_bytes!("../testdata/echo_block.wasm").to_vec()),
        )
        .mount(&server)
        .await;
    server
}

/// Default build: a registry on a private/loopback address is refused, and
/// an invalid base-URL override fails seal loudly, naming the env var.
///
/// One test, three phases (order matters — shared process env):
///  1. IP-literal loopback base (`http://127.0.0.1:{port}`) → seal error
///     citing SEC-09; the wiremock server must never be hit.
///  2. `localhost` hostname base → same refusal (URL-level `localhost`
///     check; had it slipped through, the DNS-layer resolver would refuse
///     the loopback resolution — see `wafer-net-security` unit tests).
///  3. Unparseable base → seal error naming `WAFER_RUN_REGISTRY_BASE_URL`,
///     never a silent fallback to the default registry.
#[cfg(not(feature = "allow-private-network"))]
#[tokio::test]
async fn registry_on_private_address_is_refused() {
    let server = mock_registry().await;

    // Phase 1: loopback IP literal.
    std::env::set_var(REGISTRY_BASE_URL_KEY, server.uri());
    let mut wafer = new_wafer();
    wafer.add_block_config("acme/widget@1.0.0", json!({}));
    let err = wafer
        .seal()
        .await
        .expect_err("loopback registry must be refused in a default build");
    let msg = err.to_string();
    assert!(
        msg.contains("SEC-09") && msg.contains("private/internal"),
        "seal error must cite the SSRF policy: {msg}"
    );
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty(),
        "the private registry must never be contacted"
    );

    // Phase 2: `localhost` hostname.
    let localhost_base = server.uri().replace("127.0.0.1", "localhost");
    std::env::set_var(REGISTRY_BASE_URL_KEY, &localhost_base);
    let mut wafer = new_wafer();
    wafer.add_block_config("acme/widget@1.0.0", json!({}));
    let err = wafer
        .seal()
        .await
        .expect_err("localhost registry must be refused in a default build");
    assert!(
        err.to_string().contains("private/internal"),
        "seal error must cite the SSRF policy: {err}"
    );

    // Phase 3: present-but-invalid override is a loud error naming the var.
    std::env::set_var(REGISTRY_BASE_URL_KEY, "not a url");
    let mut wafer = new_wafer();
    wafer.add_block_config("acme/widget@1.0.0", json!({}));
    let err = wafer
        .seal()
        .await
        .expect_err("invalid base URL must refuse seal");
    let msg = err.to_string();
    assert!(
        msg.contains(REGISTRY_BASE_URL_KEY),
        "seal error must name the env var: {msg}"
    );

    std::env::remove_var(REGISTRY_BASE_URL_KEY);
}

/// `allow-private-network` build: the same local wiremock registry serves
/// manifest + wasm end-to-end — seal succeeds and the block is registered.
/// Proves the escape hatch works and exercises the full download pipeline
/// (manifest fetch → version select → ABI check → wasm download → load).
#[cfg(feature = "allow-private-network")]
#[tokio::test]
async fn local_registry_works_under_escape_hatch() {
    let server = mock_registry().await;

    std::env::set_var(REGISTRY_BASE_URL_KEY, server.uri());
    let mut wafer = new_wafer();
    wafer.add_block_config("acme/widget@1.0.0", json!({}));
    let result = wafer.seal().await;
    std::env::remove_var(REGISTRY_BASE_URL_KEY);

    result.expect("seal must resolve the block from the local registry");
    assert!(
        wafer.block_names().iter().any(|n| n == "acme/widget@1.0.0"),
        "downloaded block must be registered; got {:?}",
        wafer.block_names()
    );
}
