//! SEC-019 e2e: the outbound `HttpNetworkService` follows public redirects
//! while every hop is revalidated (`ssrf_redirect_policy` + the DNS resolver).
//!
//! End-to-end proof needs a server the client will actually dial. In the
//! default build the SSRF gate blocks loopback, so — mirroring
//! `wafer-run/tests/registry_ssrf.rs` — the reachable half runs under the
//! `allow-private-network` escape hatch, where a local wiremock server on
//! 127.0.0.1 is dialable. It proves the redirect is *followed* to its final
//! hop, guarding against a regression back to `redirect::Policy::none()`
//! (which would drop the redirect and hand the caller the 302 instead).
//!
//! The security half — a redirect *target* that is private/internal is
//! rejected, and the hop count is bounded — is covered deterministically by
//! the `redirect_decision` unit tests in `wafer-net-security`, which run in
//! the default build where the enforcement is compiled in.
#![cfg(feature = "allow-private-network")]

use std::collections::HashMap;

use wafer_block_network::service::{HttpNetworkService, NetworkService, Request};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn follows_public_redirect_to_final_response() {
    let server = MockServer::start().await;
    let final_url = format!("{}/final", server.uri());

    // /start 302-redirects to /final; /final returns the real body.
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", final_url.as_str()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/final"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let svc = HttpNetworkService::with_max_response_bytes(1024);
    let req = Request {
        method: "GET".into(),
        url: format!("{}/start", server.uri()),
        headers: HashMap::new(),
        body: None,
    };

    let resp = svc
        .do_request(&req)
        .await
        .expect("public redirect should be followed to the final hop");
    assert_eq!(
        resp.status_code, 200,
        "expected the final 200, not the intermediate 302"
    );
    assert_eq!(resp.body, b"ok", "expected the final-hop body");
}
