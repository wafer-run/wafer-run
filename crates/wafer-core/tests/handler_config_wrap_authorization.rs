//! Task 6 exploit-shape tests — the config handler now authorizes
//! `config.get` and `config.set` via `ctx.check_resource_access` (host-side)
//! instead of the caller-suppliable `wrap.resource` message meta.
//!
//! `config.get` has a dual decode path (codec-encoded body, or a `key` meta
//! fallback for header-routed callers) so it can't use
//! `decode_and_authorize`'s single-decode bundling directly — it authorizes
//! manually right after resolving `key`. These tests cover both decode paths
//! to confirm the manual call still gates correctly. `config.set` is a
//! straightforward `decode_and_authorize` call.
//!
//! These tests reconstruct the meta-omission shape (WRAP metas absent on the
//! message) and assert the *ctx*, not the meta, is what gates the call: a
//! denying `Context` must produce `PermissionDenied` for both ops — and, via
//! a recording fake `ConfigService`, that the underlying service method
//! never actually ran. A granting `Context` must let the same requests
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
// Recording fake ConfigService — records every op invoked so tests can
// assert a denied request never reached the service, not just that the
// handler returned the right error.
// ---------------------------------------------------------------------------

mod config_fakes {
    use wafer_core::interfaces::config::service::ConfigService;

    use super::Calls;

    pub struct RecordingConfig {
        pub calls: Calls,
    }

    impl RecordingConfig {
        pub fn new(calls: Calls) -> Self {
            Self { calls }
        }

        fn record(&self, op: &'static str) {
            self.calls.lock().unwrap().push(op);
        }
    }

    impl ConfigService for RecordingConfig {
        fn get(&self, _key: &str) -> Option<String> {
            self.record("get");
            Some("value".into())
        }
        fn set(&self, _key: &str, _value: &str) {
            self.record("set");
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

async fn expect_success(out: OutputStream) {
    if let Err(TerminalNotResponse::Error(e)) = out.collect_buffered().await {
        panic!("expected success, got error {:?}: {}", e.code, e.message);
    }
}

// ---------------------------------------------------------------------------
// DENY cases — meta absent, ctx denies. Assert PermissionDenied AND that the
// service op never ran.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_denied_never_reaches_service_body_path() {
    let calls = new_calls();
    let svc = config_fakes::RecordingConfig::new(calls.clone());
    let req = wire::config::GetRequest {
        key: "SOLOBASE__JWT_SECRET".into(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::CONFIG_GET);

    let out = wafer_core::interfaces::config::handler::handle_message(&svc, &DenyCtx, &msg, &body);
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "get must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn get_denied_never_reaches_service_meta_fallback_path() {
    let calls = new_calls();
    let svc = config_fakes::RecordingConfig::new(calls.clone());
    // No codec-encoded body — the handler falls back to the `key` meta
    // field. That fallback must still be gated by ctx.
    let mut msg = msg_without_wrap_meta(ServiceOp::CONFIG_GET);
    msg.set_meta("key", "SOLOBASE__JWT_SECRET");

    let out = wafer_core::interfaces::config::handler::handle_message(&svc, &DenyCtx, &msg, &[]);
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "get must not run on a denied request (meta fallback path); calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn set_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = config_fakes::RecordingConfig::new(calls.clone());
    let req = wire::config::SetRequest {
        key: "SOLOBASE__JWT_SECRET".into(),
        value: "evil-value".into(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::CONFIG_SET);

    let out = wafer_core::interfaces::config::handler::handle_message(&svc, &DenyCtx, &msg, &body);
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "set must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );
}

// ---------------------------------------------------------------------------
// ALLOW case — granted ctx lets the request through to the service, for
// every op the DENY cases above cover.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn granted_ctx_allows_get_and_set() {
    let calls = new_calls();
    let svc = config_fakes::RecordingConfig::new(calls.clone());

    let get_body = codec::encode(&wire::config::GetRequest {
        key: "SOME_KEY".into(),
    })
    .unwrap();
    expect_success(wafer_core::interfaces::config::handler::handle_message(
        &svc,
        &AllowCtx,
        &msg_without_wrap_meta(ServiceOp::CONFIG_GET),
        &get_body,
    ))
    .await;

    let set_body = codec::encode(&wire::config::SetRequest {
        key: "SOME_KEY".into(),
        value: "v".into(),
    })
    .unwrap();
    expect_success(wafer_core::interfaces::config::handler::handle_message(
        &svc,
        &AllowCtx,
        &msg_without_wrap_meta(ServiceOp::CONFIG_SET),
        &set_body,
    ))
    .await;

    assert_eq!(
        *calls.lock().unwrap(),
        vec!["get", "set"],
        "every op should have reached the service exactly once, in order"
    );
}
