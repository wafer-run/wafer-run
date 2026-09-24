//! Redirects through the real stack: the `wafer-run/network` handler driving
//! `HttpNetworkService` against a live server.
//!
//! The service never follows a redirect; the handler follows it hop by hop and
//! authorizes every hop against the caller's network grant first. These tests
//! prove both halves end to end: a hop outside the grant is refused before the
//! target is contacted, and a hop inside it is followed to the final response.
//!
//! A live server has to be dialable, and in the default build the SSRF gate
//! blocks loopback — so, mirroring `wafer-run/tests/registry_ssrf.rs`, these
//! run under the `allow-private-network` escape hatch with a local wiremock
//! server on 127.0.0.1. That the SSRF gate itself runs on every hop follows
//! from each hop being a new `do_request` call; the gate on that call is
//! covered by the service's own tests in the default build.
#![cfg(feature = "allow-private-network")]

use std::collections::HashMap;

use futures::StreamExt;
use wafer_block::{
    capabilities::{Allowlist, BlockCapabilities},
    codec,
    common::{ErrorCode, ServiceOp},
    streams::{input::InputStream, output::TerminalNotResponse},
    types::{ResourceAccess, ResourceType},
    wire::network::{Request as WireRequest, ResponseHeader},
    Context, Message, OutputStream, WaferError,
};
use wafer_block_network::service::{HttpNetworkLimits, HttpNetworkService};
use wafer_core::interfaces::network::handler::handle_message;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

/// A caller whose network grant is a real capability allowlist, checked with
/// the runtime's own `allows_network_url` rule.
struct GrantCtx(BlockCapabilities);

impl GrantCtx {
    fn allowing(prefix: &str) -> Self {
        Self(BlockCapabilities {
            network: Allowlist::Only([prefix.to_string()].into()),
            ..Default::default()
        })
    }
}

#[wafer_block::wafer_async_trait]
impl Context for GrantCtx {
    async fn call_block(&self, _: &str, _: Message, _: InputStream) -> OutputStream {
        unimplemented!("not exercised by these tests")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by these tests")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by these tests")
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        unimplemented!("not exercised by these tests")
    }

    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: ResourceType,
        access: ResourceAccess,
    ) -> Result<(), WaferError> {
        if self.resource_access_admitted(resource, resource_type, access) {
            Ok(())
        } else {
            Err(WaferError::new(
                ErrorCode::PermissionDenied,
                format!("network access to {resource} denied"),
            ))
        }
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: ResourceType,
        _access: ResourceAccess,
    ) -> bool {
        resource_type == ResourceType::Network && self.0.allows_network_url(resource)
    }
}

async fn call(op: &str, ctx: &GrantCtx, url: &str) -> OutputStream {
    call_with(HttpNetworkLimits::default(), op, ctx, url).await
}

async fn call_with(limits: HttpNetworkLimits, op: &str, ctx: &GrantCtx, url: &str) -> OutputStream {
    let svc = HttpNetworkService::new(limits);
    let body = codec::encode(&WireRequest {
        method: "GET".into(),
        url: url.into(),
        headers: HashMap::new(),
        body: None,
    })
    .expect("encode request");
    handle_message(&svc, ctx, &Message::new(op), &body).await
}

/// An allowed API that redirects to a URL outside the caller's grant must not
/// hand the caller that URL's body: the call fails with `PermissionDenied`
/// and the target never receives a request. Both ops.
#[tokio::test]
async fn redirect_outside_the_grant_is_refused_before_the_target_is_contacted() {
    for op in [
        ServiceOp::NETWORK_DO_REQUEST,
        ServiceOp::NETWORK_DO_REQUEST_STREAMING,
    ] {
        let server = MockServer::start().await;
        let secret = format!("{}/internal/secret", server.uri());
        Mock::given(method("GET"))
            .and(path("/api/start"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", secret.as_str()))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/internal/secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string("secret"))
            .expect(0)
            .mount(&server)
            .await;

        let ctx = GrantCtx::allowing(&format!("{}/api", server.uri()));
        let out = call(op, &ctx, &format!("{}/api/start", server.uri())).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::PermissionDenied, "{op}: {e:?}");
            }
            other => panic!("{op}: expected PermissionDenied, got {other:?}"),
        }
        // `expect(0)` on the target is verified here (and again on drop).
        server.verify().await;
    }
}

/// A redirect that stays inside the grant is followed to the final response.
#[tokio::test]
async fn redirect_inside_the_grant_is_followed_to_the_final_response() {
    let server = MockServer::start().await;
    let final_url = format!("{}/api/final", server.uri());
    Mock::given(method("GET"))
        .and(path("/api/start"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", final_url.as_str()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/final"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let ctx = GrantCtx::allowing(&format!("{}/api", server.uri()));
    let out = call(
        ServiceOp::NETWORK_DO_REQUEST,
        &ctx,
        &format!("{}/api/start", server.uri()),
    )
    .await;
    let chunks: Vec<Vec<u8>> = out.body_stream().collect().await;
    let header: ResponseHeader = codec::decode(&chunks[0]).expect("header frame");
    assert_eq!(
        header.status_code, 200,
        "expected the final hop, not the 302"
    );
    assert_eq!(chunks[1], b"ok", "expected the final-hop body");
}

/// `request_timeout` bounds a buffered request's whole redirect chain: four
/// hops of 400 ms each stay inside a 1 s total one by one, and the call still
/// fails at about 1 s instead of running the ~1.6 s chain to the end.
#[tokio::test]
async fn buffered_total_timeout_covers_the_whole_redirect_chain() {
    let server = MockServer::start().await;
    for hop in 0..4 {
        let next = format!("{}/api/h{}", server.uri(), hop + 1);
        Mock::given(method("GET"))
            .and(path(format!("/api/h{hop}")))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", next.as_str())
                    .set_delay(std::time::Duration::from_millis(400)),
            )
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/api/h4"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let limits = HttpNetworkLimits {
        request_timeout: std::time::Duration::from_secs(1),
        ..HttpNetworkLimits::default()
    };
    let ctx = GrantCtx::allowing(&format!("{}/api", server.uri()));
    let started = std::time::Instant::now();
    let out = call_with(
        limits,
        ServiceOp::NETWORK_DO_REQUEST,
        &ctx,
        &format!("{}/api/h0", server.uri()),
    )
    .await;
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => assert_eq!(e.code, ErrorCode::Unavailable),
        other => panic!("expected the chain to time out, got {other:?}"),
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(1400),
        "the chain must stop at the 1 s total, took {elapsed:?}"
    );
}
