//! The crypto block's failure modes, driven through the real message
//! handler over [`Argon2JwtCryptoService`]: what a calling block sees when a
//! stored hash cannot be checked, and when a token expiry cannot be
//! represented.

use std::{collections::HashMap, sync::Arc};

use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceAccess, ResourceType},
    wire::crypto as wire,
    Message, WaferError,
};
use wafer_block_crypto::service::{Argon2JwtCryptoService, CryptoService};
use wafer_core::interfaces::crypto::handler;

const SECRET: &str = "test-secret-padded-to-32-bytes-or-more-for-validation-aaaaaaaaaa";
const CALLER: Option<&str> = Some("my-org/auth");

/// A bcrypt hash: a real credential format, but not one this crate writes
/// or reads.
const BCRYPT_HASH: &str = "$2b$10$N9qo8uLOickgx2ZMRZoMyeIjZAgcfl7p92ldGxad68LJZdL17lhWy";

/// `Context` granting every resource check; the handler consults nothing
/// else.
struct AllowCtx;

#[wafer_block::wafer_async_trait]
impl Context for AllowCtx {
    async fn call_block(&self, _: &str, _: Message, _: InputStream) -> OutputStream {
        unimplemented!("the crypto handler calls no block")
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

fn service() -> Arc<dyn CryptoService> {
    Arc::new(Argon2JwtCryptoService::new(SECRET.to_string()).expect("secret is long enough"))
}

/// The terminal of a handler call: `Ok(body)` or the error it carried.
async fn outcome(out: OutputStream) -> Result<Vec<u8>, WaferError> {
    match out.collect_buffered().await {
        Ok(resp) => Ok(resp.body),
        Err(TerminalNotResponse::Error(e)) => Err(e),
        Err(other) => panic!("unexpected terminal: {other:?}"),
    }
}

fn compare_body(password: &str, hash: &str) -> Vec<u8> {
    codec::encode(&wire::CompareHashRequest {
        password: password.to_string(),
        hash: hash.to_string(),
    })
    .unwrap()
}

/// A stored hash the service cannot check is a server-side fault. Reported
/// as `Unauthenticated`, a caller cannot tell it from a wrong password and
/// tells the user they mistyped — forever.
#[tokio::test]
async fn an_uncheckable_stored_hash_is_not_reported_as_a_wrong_password() {
    let svc = service();
    let msg = Message::new(ServiceOp::CRYPTO_COMPARE_HASH);
    for hash in [
        BCRYPT_HASH,
        "not-a-hash",
        "$pbkdf2-sha256$i=1000$AAECAwQFBgcICQoLDA0ODw==",
    ] {
        let body = compare_body("pw", hash);
        for (path, out) in [
            (
                "sync",
                handler::handle_message(svc.as_ref(), &AllowCtx, CALLER, &msg, &body),
            ),
            (
                "native",
                handler::handle_message_native(&svc, &AllowCtx, CALLER, &msg, &body).await,
            ),
        ] {
            let err = outcome(out)
                .await
                .expect_err(&format!("{path}: {hash:?} must not verify"));
            assert_eq!(
                err.code,
                ErrorCode::Internal,
                "{path}: {hash:?} gave {:?}: {}",
                err.code,
                err.message
            );
            assert!(
                err.message.contains("malformed password hash"),
                "{}",
                err.message
            );
        }
    }
}

/// The distinction must not cost the ordinary case: a wrong password is
/// still an ordinary `matches: false`.
#[tokio::test]
async fn a_wrong_password_is_still_a_mismatch() {
    let svc = service();
    let stored = svc.hash("right").unwrap();
    let msg = Message::new(ServiceOp::CRYPTO_COMPARE_HASH);
    let body = handler::handle_message_native(
        &svc,
        &AllowCtx,
        CALLER,
        &msg,
        &compare_body("wrong", &stored),
    )
    .await;
    let resp: wire::CompareHashResponse = codec::decode(&outcome(body).await.unwrap()).unwrap();
    assert!(!resp.matches);
}

/// `expiry_secs` comes off the wire. One past chrono's last date used to
/// panic the handler — with `panic = "abort"`, the whole process.
#[tokio::test]
async fn an_unrepresentable_expiry_is_an_error_not_a_panic() {
    let svc = service();
    let body = codec::encode(&wire::SignRequest {
        claims: HashMap::new(),
        expiry_secs: 10_000_000_000_000,
    })
    .unwrap();
    let msg = Message::new(ServiceOp::CRYPTO_SIGN);
    let err = outcome(handler::handle_message(
        svc.as_ref(),
        &AllowCtx,
        CALLER,
        &msg,
        &body,
    ))
    .await
    .expect_err("an expiry past the last date must be refused");
    assert_eq!(err.code, ErrorCode::Internal);
    assert!(
        err.message.contains("expiry out of range"),
        "{}",
        err.message
    );
}
