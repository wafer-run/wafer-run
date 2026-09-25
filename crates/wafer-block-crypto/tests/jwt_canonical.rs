//! A JWT's bytes are a function of its claims alone.
//!
//! Callers identify tokens by the bytes of the whole JWT (a refresh token is
//! stored as a hash of it), so two mints with equal claims in one second
//! must produce the same token every time, and never "usually different"
//! because a map happened to iterate in a different order. These tests build
//! the same claim set in several insertion orders and require byte-identical
//! tokens, through the signing primitive and through the crypto block's
//! message handler (the path a calling block takes).

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
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
    wire::crypto as wire,
    Message, WaferError,
};
use wafer_block_crypto::{
    primitives::{self, JwtExpPolicy},
    service::{Argon2JwtCryptoService, CryptoService},
};
use wafer_core::interfaces::crypto::handler;

const SECRET: &str = "test-secret-padded-to-32-bytes-or-more-for-validation-aaaaaaaaaa";
const CALLER: &str = "my-org/auth";

/// A refresh token's claims: every value is the same across a rotation in
/// one family, so only the encoding could tell two such tokens apart.
fn claim_pairs() -> Vec<(String, serde_json::Value)> {
    [
        ("user_id", serde_json::json!("user-1")),
        ("sub", serde_json::json!("user-1")),
        ("type", serde_json::json!("refresh")),
        ("family", serde_json::json!("fam-1")),
        ("auth_method", serde_json::json!("password")),
        ("iss", serde_json::json!("https://example.test")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// The claim pairs in every rotation plus every reversed rotation: twelve
/// insertion orders of one claim set.
fn insertion_orders() -> Vec<Vec<(String, serde_json::Value)>> {
    let base = claim_pairs();
    let mut orders = Vec::new();
    for shift in 0..base.len() {
        let mut rotated = base.clone();
        rotated.rotate_left(shift);
        let mut reversed = rotated.clone();
        reversed.reverse();
        orders.push(rotated);
        orders.push(reversed);
    }
    orders
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_secs()
}

/// `iat`/`exp` are whole seconds, so tokens compare equal only when minted
/// inside one second. Retry until a batch lands in one.
async fn mint_within_one_second<F, Fut>(mut mint_all: F) -> Vec<String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Vec<String>>,
{
    for _ in 0..10 {
        let before = now_secs();
        let tokens = mint_all().await;
        if now_secs() == before {
            return tokens;
        }
    }
    panic!("could not mint a batch of tokens inside one second in ten attempts");
}

fn payload_json(token: &str) -> String {
    let payload_b64 = token.split('.').nth(1).expect("three-part JWT");
    String::from_utf8(primitives::b64url_decode(payload_b64).expect("base64url payload"))
        .expect("UTF-8 payload")
}

fn assert_all_identical(tokens: &[String]) {
    let first = &tokens[0];
    for (i, token) in tokens.iter().enumerate() {
        assert_eq!(
            token,
            first,
            "insertion order {i} signed to different bytes:\n  {}\nvs\n  {}",
            payload_json(token),
            payload_json(first),
        );
    }
}

#[tokio::test]
async fn equal_claims_sign_to_identical_bytes_whatever_their_insertion_order() {
    let key = primitives::derive_block_key(SECRET.as_bytes(), CALLER);
    let tokens = mint_within_one_second(|| {
        let key = key.clone();
        async move {
            insertion_orders()
                .into_iter()
                .map(|pairs| {
                    primitives::jwt_sign(
                        pairs.into_iter().collect(),
                        Duration::from_secs(3600),
                        key.as_bytes(),
                    )
                    .expect("sign")
                })
                .collect()
        }
    })
    .await;
    assert_all_identical(&tokens);
}

/// The same property on the path a calling block takes: claims encoded into
/// a `crypto.sign` request, decoded by the crypto block's handler, signed by
/// the service.
#[tokio::test]
async fn equal_claims_sent_to_the_crypto_block_sign_to_identical_bytes() {
    let svc: Arc<dyn CryptoService> =
        Arc::new(Argon2JwtCryptoService::new(SECRET.to_string()).expect("secret is long enough"));
    let tokens = mint_within_one_second(|| {
        let svc = Arc::clone(&svc);
        async move {
            let mut tokens = Vec::new();
            for pairs in insertion_orders() {
                let body = codec::encode(&wire::SignRequest {
                    claims: pairs.into_iter().collect(),
                    expiry_secs: 3600,
                })
                .expect("encode");
                let msg = Message::new(ServiceOp::CRYPTO_SIGN);
                let out =
                    handler::handle_message(svc.as_ref(), &AllowCtx, Some(CALLER), &msg, &body);
                let resp: wire::SignResponse =
                    codec::decode(&outcome(out).await.expect("sign")).expect("decode");
                tokens.push(resp.token);
            }
            tokens
        }
    })
    .await;
    assert_all_identical(&tokens);
}

/// The canonical form itself: keys sorted at every depth, no whitespace,
/// array order kept. `scripts/check.sh` also runs this file with
/// serde_json's `preserve_order` on, where the nested object is only sorted
/// because the signer sorts it.
#[test]
fn the_payload_is_sorted_compact_json() {
    let key = primitives::derive_block_key(SECRET.as_bytes(), CALLER);
    let mut pairs = claim_pairs();
    pairs.push((
        "roles".to_string(),
        serde_json::json!([{ "z": 1, "a": 2 }, "admin"]),
    ));
    let token = primitives::jwt_sign(
        pairs.into_iter().collect(),
        Duration::from_secs(60),
        key.as_bytes(),
    )
    .expect("sign");

    let payload = payload_json(&token);
    let stamped: serde_json::Value = serde_json::from_str(&payload).expect("JSON payload");
    let (iat, exp) = (&stamped["iat"], &stamped["exp"]);
    assert_eq!(
        payload,
        format!(
            r#"{{"auth_method":"password","exp":{exp},"family":"fam-1","iat":{iat},"iss":"https://example.test","roles":[{{"a":2,"z":1}},"admin"],"sub":"user-1","type":"refresh","user_id":"user-1"}}"#
        ),
    );
}

/// Verification reads the signed bytes as they came: a validly signed token
/// whose payload is not in canonical order (another JWT library, a
/// hand-built fixture) still verifies. Passes before and after canonical
/// signing by design; it guards against making verification order-bound.
#[test]
fn verification_does_not_depend_on_payload_key_order() {
    let key = primitives::derive_block_key(SECRET.as_bytes(), CALLER);
    let exp = now_secs() + 3600;
    let header = primitives::b64url_encode(br#"{"typ":"JWT","alg":"HS256"}"#);
    let payload = primitives::b64url_encode(
        format!(r#"{{"type":"access","sub":"user-1","exp":{exp}}}"#).as_bytes(),
    );
    let signing_input = format!("{header}.{payload}");
    let sig = primitives::b64url_encode(&primitives::hmac_sha256(
        key.as_bytes(),
        signing_input.as_bytes(),
    ));
    let token = format!("{signing_input}.{sig}");

    let claims = primitives::jwt_verify(&token, key.as_bytes(), JwtExpPolicy::Required)
        .expect("an out-of-order payload verifies");
    assert_eq!(claims.get("sub"), Some(&serde_json::json!("user-1")));
    assert_eq!(claims.get("type"), Some(&serde_json::json!("access")));
}

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

async fn outcome(out: OutputStream) -> Result<Vec<u8>, WaferError> {
    match out.collect_buffered().await {
        Ok(resp) => Ok(resp.body),
        Err(TerminalNotResponse::Error(e)) => Err(e),
        Err(other) => panic!("unexpected terminal: {other:?}"),
    }
}
