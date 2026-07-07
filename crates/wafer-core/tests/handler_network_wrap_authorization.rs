//! Task 6 exploit-shape tests — the network handler now authorizes
//! `network.do` through `decode_and_authorize` (host-side
//! `ctx.check_resource_access`) instead of the caller-suppliable
//! `wrap.resource` message meta.
//!
//! These tests reconstruct the meta-omission shape (WRAP metas absent on the
//! message) and assert the *ctx*, not the meta, is what gates the call: a
//! denying `Context` must produce `PermissionDenied` for `network.do` — and,
//! via a recording fake `NetworkService`, that the underlying service method
//! never actually ran. A granting `Context` must let the same request
//! through and reach the service.

use std::sync::{Arc, Mutex};

use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::ResourceType,
    wire, ErrorCode, Message, WaferError,
};

// ---------------------------------------------------------------------------
// Recording fake NetworkService — records every op invoked so tests can
// assert a denied request never reached the service, not just that the
// handler returned the right error.
// ---------------------------------------------------------------------------

mod network_fakes {
    use async_trait::async_trait;
    use wafer_core::interfaces::network::service::{
        NetworkError, NetworkService, Request, Response,
    };

    use super::Calls;

    pub struct RecordingNetwork {
        pub calls: Calls,
    }

    impl RecordingNetwork {
        pub fn new(calls: Calls) -> Self {
            Self { calls }
        }
    }

    #[async_trait]
    impl NetworkService for RecordingNetwork {
        async fn do_request(&self, _req: &Request) -> Result<Response, NetworkError> {
            self.calls.lock().unwrap().push("do_request");
            Ok(Response {
                status_code: 200,
                headers: Default::default(),
                body: vec![],
            })
        }
    }
}

/// Shared call log, checked via `Arc::clone` from the test after the handler
/// call returns.
type Calls = Arc<Mutex<Vec<&'static str>>>;

fn new_calls() -> Calls {
    Arc::new(Mutex::new(Vec::new()))
}

// ---------------------------------------------------------------------------
// Context fakes
// ---------------------------------------------------------------------------

/// `Context` stub that denies every resource-access check — models a caller
/// with no WRAP grant for anything, regardless of what (if any) meta the
/// message carries.
struct DenyCtx;

#[wafer_block::wafer_async_trait]
impl Context for DenyCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    // `check_resource_access` uses the trait's fail-closed default (deny).
}

/// `Context` stub that grants every resource-access check — models a caller
/// holding a valid WRAP grant for the resource it's requesting.
struct AllowCtx;

#[wafer_block::wafer_async_trait]
impl Context for AllowCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn check_resource_access(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _is_write: bool,
    ) -> Result<(), WaferError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A bare `Message` carrying only `kind` — no `wrap.resource` /
/// `wrap.access` / `wrap.resource_type` meta at all.
fn msg_without_wrap_meta(kind: &str) -> Message {
    Message::new(kind)
}

async fn expect_permission_denied(out: OutputStream) -> WaferError {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(
                e.code,
                ErrorCode::PermissionDenied,
                "expected PERMISSION_DENIED, got {:?}: {}",
                e.code,
                e.message
            );
            e
        }
        other => panic!("expected a PermissionDenied error terminal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// DENY case — meta absent, ctx denies. Assert PermissionDenied AND that the
// service op never ran.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn do_request_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = network_fakes::RecordingNetwork::new(calls.clone());
    let req = wire::network::Request {
        method: "POST".into(),
        url: "https://internal.example.com/admin".into(),
        headers: Default::default(),
        body: None,
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::NETWORK_DO_REQUEST);

    let out =
        wafer_core::interfaces::network::handler::handle_message(&svc, &DenyCtx, &msg, &body).await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "do_request must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );
}

// ---------------------------------------------------------------------------
// ALLOW case — granted ctx lets the request through to the service.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn granted_ctx_allows_do_request() {
    let calls = new_calls();
    let svc = network_fakes::RecordingNetwork::new(calls.clone());
    let req = wire::network::Request {
        method: "GET".into(),
        url: "https://example.com/data".into(),
        headers: Default::default(),
        body: None,
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::NETWORK_DO_REQUEST);

    let out =
        wafer_core::interfaces::network::handler::handle_message(&svc, &AllowCtx, &msg, &body)
            .await;
    if let Err(TerminalNotResponse::Error(e)) = out.collect_buffered().await {
        panic!("expected success, got error {:?}: {}", e.code, e.message);
    }

    assert_eq!(
        *calls.lock().unwrap(),
        vec!["do_request"],
        "do_request should have reached the service exactly once"
    );
}
