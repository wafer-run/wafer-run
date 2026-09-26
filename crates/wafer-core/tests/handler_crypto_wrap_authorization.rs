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
//! this: it is `None` except where a granted token op needs a key to sign
//! with.

use std::{
    collections::BTreeMap,
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
    types::{ResourceAccess, ResourceType},
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

    #[wafer_block::wafer_async_trait]
    impl CryptoService for RecordingCrypto {
        async fn hash(&self, _password: &str) -> Result<String, CryptoError> {
            self.record("hash");
            Ok("hash".into())
        }
        async fn compare_hash(&self, _password: &str, _hash: &str) -> Result<(), CryptoError> {
            self.record("compare_hash");
            Ok(())
        }
        async fn sign_for(
            &self,
            _block_id: &str,
            _claims: std::collections::BTreeMap<String, serde_json::Value>,
            _expiry: std::time::Duration,
        ) -> Result<String, CryptoError> {
            self.record("sign");
            Ok("token".into())
        }
        async fn verify_for(
            &self,
            _block_id: &str,
            _token: &str,
        ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, CryptoError> {
            self.record("verify");
            Ok(Default::default())
        }
        async fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
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
    // Denies every access, as the trait's default `check_resource_access` does.
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: wafer_block::types::ResourceType,
        _access: wafer_block::types::ResourceAccess,
    ) -> bool {
        false
    }
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
        _access: ResourceAccess,
    ) -> Result<(), WaferError> {
        Ok(())
    }
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _access: ResourceAccess,
    ) -> bool {
        true
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
        claims: BTreeMap::from([("sub".to_string(), serde_json::json!("evil-user"))]),
        expiry_secs: 3600,
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::CRYPTO_SIGN);

    let out =
        wafer_core::interfaces::crypto::handler::handle_message(&svc, &DenyCtx, None, &msg, &body)
            .await;
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
        wafer_core::interfaces::crypto::handler::handle_message(&svc, &DenyCtx, None, &msg, &body)
            .await;
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
        wafer_core::interfaces::crypto::handler::handle_message(&svc, &DenyCtx, None, &msg, &body)
            .await;
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
        claims: BTreeMap::new(),
        expiry_secs: 3600,
    })
    .unwrap();
    expect_success(
        wafer_core::interfaces::crypto::handler::handle_message(
            &svc,
            &AllowCtx,
            Some("test/caller"),
            &msg_without_wrap_meta(ServiceOp::CRYPTO_SIGN),
            &sign_body,
        )
        .await,
    )
    .await;

    let hash_body = codec::encode(&wire::crypto::HashRequest {
        password: "hunter2".into(),
    })
    .unwrap();
    expect_success(
        wafer_core::interfaces::crypto::handler::handle_message(
            &svc,
            &AllowCtx,
            None,
            &msg_without_wrap_meta(ServiceOp::CRYPTO_HASH),
            &hash_body,
        )
        .await,
    )
    .await;

    let random_bytes_body = codec::encode(&wire::crypto::RandomBytesRequest { n: 16 }).unwrap();
    expect_success(
        wafer_core::interfaces::crypto::handler::handle_message(
            &svc,
            &AllowCtx,
            None,
            &msg_without_wrap_meta(ServiceOp::CRYPTO_RANDOM_BYTES),
            &random_bytes_body,
        )
        .await,
    )
    .await;

    assert_eq!(
        *calls.lock().unwrap(),
        vec!["sign", "hash", "random_bytes"],
        "every op should have reached the service exactly once, in order"
    );
}

// ---------------------------------------------------------------------------
// compare_hash — the one op the DENY/ALLOW cases above leave out.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compare_hash_is_gated_by_the_ctx() {
    let calls = new_calls();
    let svc = crypto_fakes::RecordingCrypto::new(calls.clone());
    let body = codec::encode(&wire::crypto::CompareHashRequest {
        password: "hunter2".into(),
        hash: "hash".into(),
    })
    .unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::CRYPTO_COMPARE_HASH);

    expect_permission_denied(
        wafer_core::interfaces::crypto::handler::handle_message(&svc, &DenyCtx, None, &msg, &body)
            .await,
    )
    .await;
    assert!(
        calls.lock().unwrap().is_empty(),
        "compare_hash must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );

    expect_success(
        wafer_core::interfaces::crypto::handler::handle_message(&svc, &AllowCtx, None, &msg, &body)
            .await,
    )
    .await;
    assert_eq!(*calls.lock().unwrap(), vec!["compare_hash"]);
}

// ---------------------------------------------------------------------------
// Tokens are keyed per calling block. A granted token op that arrives with no
// calling block has no key to use and is refused — it never falls back to a
// shared master key.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn granted_token_ops_without_a_caller_are_refused_before_the_service() {
    let calls = new_calls();
    let svc = crypto_fakes::RecordingCrypto::new(calls.clone());

    let sign_body = codec::encode(&wire::crypto::SignRequest {
        claims: BTreeMap::new(),
        expiry_secs: 3600,
    })
    .unwrap();
    let err = expect_permission_denied(
        wafer_core::interfaces::crypto::handler::handle_message(
            &svc,
            &AllowCtx,
            None,
            &msg_without_wrap_meta(ServiceOp::CRYPTO_SIGN),
            &sign_body,
        )
        .await,
    )
    .await;
    assert!(err.message.contains("calling block"), "{}", err.message);

    let verify_body = codec::encode(&wire::crypto::VerifyRequest {
        token: "a.b.c".into(),
    })
    .unwrap();
    let err = expect_permission_denied(
        wafer_core::interfaces::crypto::handler::handle_message(
            &svc,
            &AllowCtx,
            None,
            &msg_without_wrap_meta(ServiceOp::CRYPTO_VERIFY),
            &verify_body,
        )
        .await,
    )
    .await;
    assert!(err.message.contains("calling block"), "{}", err.message);

    assert!(
        calls.lock().unwrap().is_empty(),
        "no token op may reach the service without a key; calls = {:?}",
        calls.lock().unwrap()
    );
}
