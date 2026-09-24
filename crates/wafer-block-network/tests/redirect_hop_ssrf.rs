//! A redirect hop to an internal address is refused by the SSRF gate.
//!
//! Runs in the default (enforcing) build. A public first hop cannot be served
//! from a test, so the first hop is scripted: `FirstHopThen` answers the
//! starting URL with a 302 and hands every later hop to the real
//! `HttpNetworkService`. The handler follows the 302 and issues the hop through
//! that service, whose SSRF gate must refuse it. The caller's grant is `Any`,
//! so the refusal can only come from the SSRF gate, not from the grant check.
#![cfg(not(feature = "allow-private-network"))]

use std::{collections::HashMap, time::Duration};

use wafer_block::{
    capabilities::{Allowlist, BlockCapabilities},
    codec,
    common::{ErrorCode, ServiceOp},
    streams::{input::InputStream, output::TerminalNotResponse},
    types::{ResourceAccess, ResourceType},
    wire::network::Request as WireRequest,
    Context, Message, OutputStream, WaferError,
};
use wafer_block_network::service::{
    HttpNetworkLimits, HttpNetworkService, NetworkError, NetworkService, Request, Response,
};
use wafer_core::interfaces::network::handler::handle_message;

const START: &str = "https://redirector.example/start";

/// A caller holding the unrestricted network grant.
struct AnyNetwork(BlockCapabilities);

#[wafer_block::wafer_async_trait]
impl Context for AnyNetwork {
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
            Err(WaferError::new(ErrorCode::PermissionDenied, "denied"))
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

/// Answers [`START`] with a 302 to `target`; every other URL goes to the real
/// service.
struct FirstHopThen {
    target: String,
    real: HttpNetworkService,
}

#[wafer_block::wafer_async_trait]
impl NetworkService for FirstHopThen {
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
        if req.url == START {
            return Ok(Response {
                status_code: 302,
                headers: HashMap::from([("location".to_string(), vec![self.target.clone()])]),
                body: Vec::new(),
            });
        }
        self.real.do_request(req).await
    }
}

async fn redirect_to(target: &str) -> WaferError {
    let svc = FirstHopThen {
        target: target.to_string(),
        // Short timeouts so a missing gate fails fast instead of hanging on
        // an unroutable address.
        real: HttpNetworkService::new(HttpNetworkLimits {
            connect_timeout: Duration::from_secs(1),
            read_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(2),
            ..HttpNetworkLimits::default()
        }),
    };
    let ctx = AnyNetwork(BlockCapabilities {
        network: Allowlist::Any,
        ..Default::default()
    });
    let body = codec::encode(&WireRequest {
        method: "GET".into(),
        url: START.into(),
        headers: HashMap::new(),
        body: None,
    })
    .expect("encode request");
    let out = handle_message(
        &svc,
        &ctx,
        &Message::new(ServiceOp::NETWORK_DO_REQUEST),
        &body,
    )
    .await;
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => e,
        other => panic!("redirect to {target} must fail, got {other:?}"),
    }
}

#[tokio::test]
async fn redirect_hop_to_cloud_metadata_is_refused_by_the_ssrf_gate() {
    let e = redirect_to("http://169.254.169.254/latest/meta-data/").await;
    assert_eq!(e.code, ErrorCode::Unavailable, "{e:?}");
    assert!(e.message.contains("private/internal"), "{e:?}");
}

#[tokio::test]
async fn redirect_hop_to_localhost_is_refused_by_the_ssrf_gate() {
    let e = redirect_to("http://localhost/admin").await;
    assert_eq!(e.code, ErrorCode::Unavailable, "{e:?}");
    assert!(e.message.contains("private/internal"), "{e:?}");
}
