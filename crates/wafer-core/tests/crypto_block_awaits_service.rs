//! The crypto block awaits its `CryptoService`: a service whose password
//! hash or check finishes only after an await point — the shape of a backend
//! that runs Argon2 in a Cloudflare Durable Object — answers through the real
//! block, and the block's call stays pending, holding no thread, until the
//! service resolves.
//!
//! A service that fails after the await with `CryptoError::Unavailable` —
//! the Durable Object could not be reached — makes the block answer
//! `ErrorCode::Unavailable` (HTTP 503), not `Internal`.
//!
//! `tests/wasm32_crypto_block` runs the same scenarios on
//! wasm32-unknown-unknown, where the block used to call the handler
//! synchronously.

use std::sync::{Arc, Mutex};

use futures::channel::oneshot;
use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    http_codec::error_code_to_http_status,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceAccess, ResourceType},
    wire::crypto as wire,
    Block, ErrorCode, Message, WaferError,
};
use wafer_core::{
    interfaces::crypto::service::{CryptoError, CryptoService},
    service_blocks::crypto::CryptoBlock,
};

/// A service whose password ops resolve only when the test releases them,
/// with the answer the test sends.
struct GatedCrypto {
    hash: Mutex<Option<oneshot::Receiver<Result<String, CryptoError>>>>,
    compare: Mutex<Option<oneshot::Receiver<Result<(), CryptoError>>>>,
}

impl GatedCrypto {
    fn take<T>(slot: &Mutex<Option<oneshot::Receiver<T>>>) -> oneshot::Receiver<T> {
        slot.lock().unwrap().take().expect("each gate is used once")
    }
}

#[wafer_core::wafer_async_trait]
impl CryptoService for GatedCrypto {
    async fn hash(&self, _password: &str) -> Result<String, CryptoError> {
        Self::take(&self.hash)
            .await
            .unwrap_or_else(|_| Err(CryptoError::HashError("gate dropped".into())))
    }

    async fn compare_hash(&self, _password: &str, _hash: &str) -> Result<(), CryptoError> {
        Self::take(&self.compare)
            .await
            .unwrap_or_else(|_| Err(CryptoError::Other("gate dropped".into())))
    }

    async fn sign_for(
        &self,
        _block_id: &str,
        _claims: std::collections::BTreeMap<String, serde_json::Value>,
        _expiry: std::time::Duration,
    ) -> Result<String, CryptoError> {
        unreachable!("only the password ops are exercised")
    }

    async fn verify_for(
        &self,
        _block_id: &str,
        _token: &str,
    ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, CryptoError> {
        unreachable!("only the password ops are exercised")
    }

    async fn random_bytes(&self, _n: usize) -> Result<Vec<u8>, CryptoError> {
        unreachable!("only the password ops are exercised")
    }
}

/// Grants every resource check; the crypto handler consults nothing else.
struct AllowCtx;

#[wafer_core::wafer_async_trait]
impl Context for AllowCtx {
    async fn call_block(&self, _: &str, _: Message, _: InputStream) -> OutputStream {
        unimplemented!("the crypto block calls no block")
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    fn config_get(&self, _: &str) -> Option<&str> {
        None
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(AllowCtx)
    }
    fn check_resource_access(
        &self,
        _: &str,
        _: ResourceType,
        _: ResourceAccess,
    ) -> Result<(), WaferError> {
        Ok(())
    }
    fn resource_access_admitted(&self, _: &str, _: ResourceType, _: ResourceAccess) -> bool {
        true
    }
}

async fn body_of(out: OutputStream) -> Vec<u8> {
    match out.collect_buffered().await {
        Ok(resp) => resp.body,
        Err(TerminalNotResponse::Error(e)) => panic!("{:?}: {}", e.code, e.message),
        Err(other) => panic!("unexpected terminal: {other:?}"),
    }
}

#[tokio::test]
async fn the_block_waits_for_a_hash_that_completes_after_an_await_point() {
    let (release, gate) = oneshot::channel();
    let block = CryptoBlock::new(Arc::new(GatedCrypto {
        hash: Mutex::new(Some(gate)),
        compare: Mutex::new(None),
    }));
    let body = codec::encode(&wire::HashRequest {
        password: "correct horse".into(),
    })
    .unwrap();

    let mut call = Box::pin(block.handle(
        &AllowCtx,
        Message::new(ServiceOp::CRYPTO_HASH),
        InputStream::from_bytes(body),
    ));
    assert!(
        futures::poll!(call.as_mut()).is_pending(),
        "the block must wait for the service, not answer before it has"
    );

    release.send(Ok("$gated$hash".to_string())).unwrap();
    let resp: wire::HashResponse = codec::decode(&body_of(call.await).await).unwrap();
    assert_eq!(resp.hash, "$gated$hash");
}

#[tokio::test]
async fn the_block_waits_for_a_password_check_that_completes_after_an_await_point() {
    for (verdict, matches) in [(true, true), (false, false)] {
        let (release, gate) = oneshot::channel();
        let block = CryptoBlock::new(Arc::new(GatedCrypto {
            hash: Mutex::new(None),
            compare: Mutex::new(Some(gate)),
        }));
        let body = codec::encode(&wire::CompareHashRequest {
            password: "correct horse".into(),
            hash: "$gated$hash".into(),
        })
        .unwrap();

        let mut call = Box::pin(block.handle(
            &AllowCtx,
            Message::new(ServiceOp::CRYPTO_COMPARE_HASH),
            InputStream::from_bytes(body),
        ));
        assert!(
            futures::poll!(call.as_mut()).is_pending(),
            "the block must wait for the service, not answer before it has"
        );

        release
            .send(if verdict {
                Ok(())
            } else {
                Err(CryptoError::PasswordMismatch)
            })
            .unwrap();
        let resp: wire::CompareHashResponse = codec::decode(&body_of(call.await).await).unwrap();
        assert_eq!(resp.matches, matches);
    }
}

/// The error the block answered with; panics if it answered anything else.
async fn error_of(out: OutputStream) -> WaferError {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => e,
        Ok(_) => panic!("the block answered, but the service failed"),
        Err(other) => panic!("unexpected terminal: {other:?}"),
    }
}

fn unreachable_hasher() -> CryptoError {
    CryptoError::Unavailable("hasher durable object: fetch timed out".into())
}

#[tokio::test]
async fn an_unreachable_hash_backend_answers_unavailable() {
    let (release, gate) = oneshot::channel();
    let block = CryptoBlock::new(Arc::new(GatedCrypto {
        hash: Mutex::new(Some(gate)),
        compare: Mutex::new(None),
    }));
    let body = codec::encode(&wire::HashRequest {
        password: "correct horse".into(),
    })
    .unwrap();

    let call = block.handle(
        &AllowCtx,
        Message::new(ServiceOp::CRYPTO_HASH),
        InputStream::from_bytes(body),
    );
    release.send(Err(unreachable_hasher())).unwrap();
    let err = error_of(call.await).await;
    assert_eq!(err.code, ErrorCode::Unavailable, "{}", err.message);
    assert_eq!(error_code_to_http_status(&err.code), 503);
}

#[tokio::test]
async fn an_unreachable_password_check_backend_answers_unavailable() {
    let (release, gate) = oneshot::channel();
    let block = CryptoBlock::new(Arc::new(GatedCrypto {
        hash: Mutex::new(None),
        compare: Mutex::new(Some(gate)),
    }));
    let body = codec::encode(&wire::CompareHashRequest {
        password: "correct horse".into(),
        hash: "$gated$hash".into(),
    })
    .unwrap();

    let call = block.handle(
        &AllowCtx,
        Message::new(ServiceOp::CRYPTO_COMPARE_HASH),
        InputStream::from_bytes(body),
    );
    release.send(Err(unreachable_hasher())).unwrap();
    let err = error_of(call.await).await;
    assert_eq!(err.code, ErrorCode::Unavailable, "{}", err.message);
    assert_eq!(error_code_to_http_status(&err.code), 503);
}
