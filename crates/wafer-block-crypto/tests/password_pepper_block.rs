//! The password pepper end to end: the crypto block's `lifecycle(Init)`
//! reads its declared config and configures [`Argon2JwtCryptoService`], and
//! the real message handler then hashes and verifies under it.

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
    Block, LifecycleEvent, LifecycleType, Message, WaferError,
};
use wafer_block_crypto::service::{Argon2JwtCryptoService, CryptoService};
use wafer_core::{
    interfaces::crypto::handler,
    service_blocks::crypto::{
        CryptoBlock, PASSWORD_PEPPER_KEY, PASSWORD_PEPPER_PREVIOUS_KEY, PASSWORD_PEPPER_REQUIRED,
    },
};

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

/// A service behind a block Init-ed with `config` — the JSON map the runtime
/// builds from the block's declared keys.
async fn init(config: &[(&str, &str)]) -> Result<Arc<dyn CryptoService>, WaferError> {
    let service: Arc<dyn CryptoService> =
        Arc::new(Argon2JwtCryptoService::new(SECRET.to_string()).expect("secret is long enough"));
    let block = CryptoBlock::new(Arc::clone(&service));
    let data: HashMap<&str, &str> = config.iter().copied().collect();
    block
        .lifecycle(
            &AllowCtx,
            LifecycleEvent {
                event_type: LifecycleType::Init,
                data: serde_json::to_vec(&data).unwrap(),
            },
        )
        .await?;
    Ok(service)
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
async fn init_config_peppers_hashes_through_the_handler() {
    let svc = init(&[(PASSWORD_PEPPER_KEY, KEY_1_B64)])
        .await
        .expect("init");
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
    let stored = hash(
        &init(&[(PASSWORD_PEPPER_KEY, KEY_1_B64)]).await.unwrap(),
        "pw",
    )
    .await;
    for svc in [
        init(&[]).await.unwrap(),
        init(&[(PASSWORD_PEPPER_KEY, KEY_2_B64)]).await.unwrap(),
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
async fn rotation_through_init_config() {
    let old = hash(
        &init(&[(PASSWORD_PEPPER_KEY, KEY_1_B64)]).await.unwrap(),
        "pw",
    )
    .await;
    let svc = init(&[
        (PASSWORD_PEPPER_KEY, KEY_2_B64),
        (PASSWORD_PEPPER_PREVIOUS_KEY, KEY_1_B64),
    ])
    .await
    .unwrap();
    assert!(compare(&svc, "pw", &old).await.unwrap());
    assert!(hash(&svc, "pw").await.contains(KEY_2_ID));
}

#[tokio::test]
async fn unpeppered_hashes_verify_until_a_pepper_is_required() {
    let legacy = hash(&init(&[]).await.unwrap(), "pw").await;
    assert!(legacy.starts_with("$argon2id$"), "{legacy}");
    let optional = init(&[(PASSWORD_PEPPER_KEY, KEY_1_B64)]).await.unwrap();
    assert!(compare(&optional, "pw", &legacy).await.unwrap());

    let required = init(&[
        (PASSWORD_PEPPER_KEY, KEY_1_B64),
        (PASSWORD_PEPPER_REQUIRED, "true"),
    ])
    .await
    .unwrap();
    let err = compare(&required, "pw", &legacy)
        .await
        .expect_err("refused");
    assert_eq!(err.code, ErrorCode::Internal, "{err}");
    assert!(err.message.contains("not peppered"), "{err}");
}

/// Bad settings fail the Init — permanently, and without echoing the key.
#[tokio::test]
async fn invalid_settings_fail_init() {
    // 31 bytes.
    let short = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHg==";
    for (config, expect) in [
        (vec![(PASSWORD_PEPPER_KEY, short)], "at least 32 bytes"),
        (vec![(PASSWORD_PEPPER_KEY, "not base64!")], "base64"),
        (vec![(PASSWORD_PEPPER_REQUIRED, "true")], "required"),
        (
            vec![(PASSWORD_PEPPER_PREVIOUS_KEY, KEY_1_B64)],
            "without a current key",
        ),
        (
            vec![(PASSWORD_PEPPER_REQUIRED, "yes")],
            PASSWORD_PEPPER_REQUIRED,
        ),
    ] {
        let err = match init(&config).await {
            Ok(_) => panic!("{config:?} must fail init"),
            Err(e) => e,
        };
        assert!(
            matches!(
                err.code,
                ErrorCode::FailedPrecondition | ErrorCode::InvalidArgument
            ),
            "{err}"
        );
        assert!(err.message.contains(expect), "{err}");
        assert!(!err.message.contains("not base64!") && !err.message.contains(short));
    }
}
