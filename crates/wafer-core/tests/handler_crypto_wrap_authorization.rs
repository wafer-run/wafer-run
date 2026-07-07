//! Task 6 exploit-shape tests — the crypto handler now authorizes every op
//! arm through `decode_and_authorize` (host-side `ctx.check_resource_access`)
//! instead of the local SEC-003 `wrap.resource` meta comparison
//! (`check_op`, removed by this task).
//!
//! These tests reconstruct the meta-omission shape (WRAP metas absent on the
//! message) and assert the *ctx*, not the meta, is what gates the call: a
//! denying `Context` must produce `PermissionDenied` for `crypto.sign` (and,
//! for good measure, `crypto.hash` / `crypto.random_bytes`) — and, via a
//! recording fake `CryptoService`, that the underlying service method never
//! actually ran. A granting `Context` must let the same requests through and
//! reach the service. `caller_id` (HKDF key derivation) is orthogonal to
//! this and stays `None` throughout.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

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
// Recording fake CryptoService — records every op invoked so tests can
// assert a denied request never reached the service, not just that the
// handler returned the right error.
// ---------------------------------------------------------------------------

mod crypto_fakes {
    use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};

    use super::Calls;

    pub struct RecordingCrypto {
        pub calls: Calls,
    }

    impl RecordingCrypto {
        pub fn new(calls: Calls) -> Self {
            Self { calls }
        }

        fn record(&self, op: &'static str) {
            self.calls.lock().unwrap().push(op);
        }
    }

    impl CryptoService for RecordingCrypto {
        fn hash(&self, _password: &str) -> Result<String, CryptoError> {
            self.record("hash");
            Ok("hash".into())
        }
        fn compare_hash(&self, _password: &str, _hash: &str) -> Result<(), CryptoError> {
            self.record("compare_hash");
            Ok(())
        }
        fn sign(
            &self,
            _claims: std::collections::HashMap<String, serde_json::Value>,
            _expiry: std::time::Duration,
        ) -> Result<String, CryptoError> {
            self.record("sign");
            Ok("token".into())
        }
        fn verify(
            &self,
            _token: &str,
        ) -> Result<std::collections::HashMap<String, serde_json::Value>, CryptoError> {
            self.record("verify");
            Ok(Default::default())
        }
        fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
            self.record("random_bytes");
            Ok(vec![0; n])
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
async fn sign_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = crypto_fakes::RecordingCrypto::new(calls.clone());
    let req = wire::crypto::SignRequest {
        claims: HashMap::from([("sub".to_string(), serde_json::json!("evil-user"))]),
        expiry_secs: 3600,
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::CRYPTO_SIGN);

    let out =
        wafer_core::interfaces::crypto::handler::handle_message(&svc, &DenyCtx, None, &msg, &body);
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "sign must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn hash_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = crypto_fakes::RecordingCrypto::new(calls.clone());
    let req = wire::crypto::HashRequest {
        password: "hunter2".into(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::CRYPTO_HASH);

    let out =
        wafer_core::interfaces::crypto::handler::handle_message(&svc, &DenyCtx, None, &msg, &body);
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "hash must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn random_bytes_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = crypto_fakes::RecordingCrypto::new(calls.clone());
    let req = wire::crypto::RandomBytesRequest { n: 32 };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::CRYPTO_RANDOM_BYTES);

    let out =
        wafer_core::interfaces::crypto::handler::handle_message(&svc, &DenyCtx, None, &msg, &body);
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "random_bytes must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );
}

// ---------------------------------------------------------------------------
// ALLOW case — granted ctx lets the request through to the service, for
// every op the DENY cases above cover.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn granted_ctx_allows_sign_hash_and_random_bytes() {
    let calls = new_calls();
    let svc = crypto_fakes::RecordingCrypto::new(calls.clone());

    let sign_body = codec::encode(&wire::crypto::SignRequest {
        claims: HashMap::new(),
        expiry_secs: 3600,
    })
    .unwrap();
    expect_success(wafer_core::interfaces::crypto::handler::handle_message(
        &svc,
        &AllowCtx,
        None,
        &msg_without_wrap_meta(ServiceOp::CRYPTO_SIGN),
        &sign_body,
    ))
    .await;

    let hash_body = codec::encode(&wire::crypto::HashRequest {
        password: "hunter2".into(),
    })
    .unwrap();
    expect_success(wafer_core::interfaces::crypto::handler::handle_message(
        &svc,
        &AllowCtx,
        None,
        &msg_without_wrap_meta(ServiceOp::CRYPTO_HASH),
        &hash_body,
    ))
    .await;

    let random_bytes_body = codec::encode(&wire::crypto::RandomBytesRequest { n: 16 }).unwrap();
    expect_success(wafer_core::interfaces::crypto::handler::handle_message(
        &svc,
        &AllowCtx,
        None,
        &msg_without_wrap_meta(ServiceOp::CRYPTO_RANDOM_BYTES),
        &random_bytes_body,
    ))
    .await;

    assert_eq!(
        *calls.lock().unwrap(),
        vec!["sign", "hash", "random_bytes"],
        "every op should have reached the service exactly once, in order"
    );
}
