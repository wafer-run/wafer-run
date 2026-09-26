//! The password pepper through the crypto block's real message handler, over
//! an [`Argon2JwtCryptoService`] built with pepper keys the way an embedder
//! builds it: from values it holds outside the database.

use std::sync::Arc;

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
use wafer_block_crypto::service::{Argon2JwtCryptoService, CryptoService, PasswordPeppers};
use wafer_core::interfaces::crypto::handler;

const SECRET: &str = "test-secret-padded-to-32-bytes-or-more-for-validation-aaaaaaaaaa";
const KEY_1_B64: &str = "ICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj8=";
const KEY_1_ID: &str = "b57af81f66f733f4";
const KEY_2_B64: &str = "QEFCQ0RFRkdISUpLTE1OT1BRUlNUVVZXWFlaW1xdXl8=";
const KEY_2_ID: &str = "d78a88c0339a5e5d";
const CALLER: Option<&str> = Some("my-org/auth");

/// `Context` granting every resource check.
struct AllowCtx;

#[wafer_block::wafer_async_trait]
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

/// A service peppered with `current` and `previous` (either may be unset).
fn service(
    current: Option<&str>,
    previous: Option<&str>,
    required: bool,
) -> Arc<dyn CryptoService> {
    let peppers = PasswordPeppers::from_config(current, previous, required).expect("valid keys");
    Arc::new(
        Argon2JwtCryptoService::new(SECRET.to_string())
            .expect("secret is long enough")
            .with_password_peppers(peppers)
            .expect("argon2 takes a pepper"),
    )
}

async fn outcome(out: OutputStream) -> Result<Vec<u8>, WaferError> {
    match out.collect_buffered().await {
        Ok(resp) => Ok(resp.body),
        Err(TerminalNotResponse::Error(e)) => Err(e),
        Err(other) => panic!("unexpected terminal: {other:?}"),
    }
}

async fn hash(svc: &Arc<dyn CryptoService>, password: &str) -> String {
    let body = codec::encode(&wire::HashRequest {
        password: password.to_string(),
    })
    .unwrap();
    let msg = Message::new(ServiceOp::CRYPTO_HASH);
    let out = handler::handle_message(svc.as_ref(), &AllowCtx, CALLER, &msg, &body).await;
    codec::decode::<wire::HashResponse>(&outcome(out).await.expect("hash"))
        .unwrap()
        .hash
}

async fn compare(
    svc: &Arc<dyn CryptoService>,
    password: &str,
    stored: &str,
) -> Result<bool, WaferError> {
    let body = codec::encode(&wire::CompareHashRequest {
        password: password.to_string(),
        hash: stored.to_string(),
    })
    .unwrap();
    let msg = Message::new(ServiceOp::CRYPTO_COMPARE_HASH);
    let out = handler::handle_message(svc.as_ref(), &AllowCtx, CALLER, &msg, &body).await;
    Ok(
        codec::decode::<wire::CompareHashResponse>(&outcome(out).await?)
            .unwrap()
            .matches,
    )
}

#[tokio::test]
async fn a_peppered_service_hashes_and_verifies_through_the_handler() {
    let svc = service(Some(KEY_1_B64), None, false);
    let stored = hash(&svc, "pw").await;
    assert!(
        stored.starts_with("$argon2id-hmac-sha256$") && stored.contains(KEY_1_ID),
        "{stored}"
    );
    assert!(compare(&svc, "pw", &stored).await.unwrap());
    assert!(!compare(&svc, "wrong", &stored).await.unwrap());
}

/// A deployment that lost its key must not tell users they mistyped: the
/// handler reports a server fault, not `matches: false`.
#[tokio::test]
async fn a_missing_pepper_key_is_a_server_fault_not_a_wrong_password() {
    let stored = hash(&service(Some(KEY_1_B64), None, false), "pw").await;
    for svc in [
        service(None, None, false),
        service(Some(KEY_2_B64), None, false),
    ] {
        let err = compare(&svc, "pw", &stored)
            .await
            .expect_err("must not answer matches");
        assert_eq!(err.code, ErrorCode::Internal, "{err}");
        assert!(
            err.message.contains("password pepper") && err.message.contains(KEY_1_ID),
            "{err}"
        );
    }
}

#[tokio::test]
async fn rotation_through_the_handler() {
    let old = hash(&service(Some(KEY_1_B64), None, false), "pw").await;
    let svc = service(Some(KEY_2_B64), Some(KEY_1_B64), false);
    assert!(compare(&svc, "pw", &old).await.unwrap());
    assert!(hash(&svc, "pw").await.contains(KEY_2_ID));
}

#[tokio::test]
async fn unpeppered_hashes_verify_until_a_pepper_is_required() {
    let legacy = hash(&service(None, None, false), "pw").await;
    assert!(legacy.starts_with("$argon2id$"), "{legacy}");
    assert!(
        compare(&service(Some(KEY_1_B64), None, false), "pw", &legacy)
            .await
            .unwrap()
    );
    let err = compare(&service(Some(KEY_1_B64), None, true), "pw", &legacy)
        .await
        .expect_err("refused");
    assert_eq!(err.code, ErrorCode::Internal, "{err}");
    assert!(err.message.contains("not peppered"), "{err}");
}
