use std::{collections::BTreeMap, time::Duration};

// Re-export the trait and error from wafer-core.
pub use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};
#[cfg(not(target_arch = "wasm32"))]
use zeroize::Zeroizing;

// Re-exported from `primitives` (where the recommendation is documented as
// part of the shared policy) so existing `service::MIN_JWT_SECRET_LEN`
// imports keep working.
pub use crate::primitives::MIN_JWT_SECRET_LEN;
use crate::primitives::{self, JwtExpPolicy};
// Re-exported so a caller configuring the service does not need a second
// import path for the two types `with_password_scheme` takes.
pub use crate::primitives::{Argon2Cost, PasswordScheme};

/// Argon2 + JWT crypto service.
///
/// Thin policy wrapper over [`crate::primitives`]: password hashing under a
/// selectable [`PasswordScheme`] (argon2id at [`Argon2Cost::Default`] unless
/// told otherwise), HS256 JWT sign/verify with [`JwtExpPolicy::Required`],
/// and per-block keys derived via [`primitives::derive_block_key`]. All pure
/// Rust, wasm32-compatible.
pub struct Argon2JwtCryptoService {
    jwt_secret: String,
    /// Scheme used to WRITE new password hashes. Verification does not
    /// consult it — see this type's [`CryptoService::compare_hash`].
    password_scheme: PasswordScheme,
}

impl Argon2JwtCryptoService {
    /// Build a service from a JWT signing secret.
    ///
    /// Fails when the secret is shorter than [`MIN_JWT_SECRET_LEN`] bytes
    /// — a weak secret here defeats the security of every token signed
    /// by the runtime, so this is a fail-fast at construction rather
    /// than an issue surfaced per-request.
    ///
    /// Passwords are hashed with argon2id at [`Argon2Cost::Default`]; call
    /// [`Self::with_password_scheme`] to choose otherwise.
    pub fn new(jwt_secret: String) -> Result<Self, CryptoError> {
        if jwt_secret.len() < MIN_JWT_SECRET_LEN {
            return Err(CryptoError::Other(format!(
                "JWT secret must be at least {MIN_JWT_SECRET_LEN} bytes (HS256 requires \
                 ≥ HMAC-SHA256 output length per RFC 2104); got {}",
                jwt_secret.len()
            )));
        }
        Ok(Self {
            jwt_secret,
            password_scheme: PasswordScheme::default(),
        })
    }

    /// Choose the algorithm this service uses when it **writes** a new
    /// password hash.
    ///
    /// This exists because no one scheme fits every target this runtime
    /// ships to, so the deployment, not the library, has to pick. Measured
    /// CPU per hash in wasm32 under V8: argon2id at [`Argon2Cost::Default`]
    /// about 17-35 ms, at [`Argon2Cost::Constrained`] about 3-8 ms, PBKDF2 at
    /// [`crate::primitives::PBKDF2_SHA256_RECOMMENDED_ITERATIONS`] about
    /// 180 ms. Under a CPU budget such as a Cloudflare Worker's 10 ms on the
    /// Free plan only `Argon2(Constrained)` fits. The choice changes nothing
    /// else: JWT signing, per-block key derivation and randomness are
    /// unaffected.
    ///
    /// # It does not invalidate stored credentials
    ///
    /// The scheme applies to [`CryptoService::hash`] only.
    /// [`CryptoService::compare_hash`] dispatches on the format of the hash
    /// it is handed, so every credential already in the database keeps
    /// verifying, whichever scheme wrote it. Selecting a scheme changes what
    /// new and re-set passwords look like; it is not a password reset.
    ///
    /// Old hashes are also not upgraded: a credential keeps the scheme and
    /// cost it was written with until something rewrites it. Re-hash on the
    /// next successful login if you want them migrated.
    pub fn with_password_scheme(mut self, scheme: PasswordScheme) -> Self {
        self.password_scheme = scheme;
        self
    }
}

#[wafer_core::wafer_async_trait]
impl CryptoService for Argon2JwtCryptoService {
    /// On a native host the derivation runs on a dedicated thread (see
    /// `offload`), so a hash never stalls the thread polling it, whatever the
    /// executor; on wasm32 it runs inline.
    async fn hash(&self, password: &str) -> Result<String, CryptoError> {
        let scheme = self.password_scheme;
        #[cfg(not(target_arch = "wasm32"))]
        {
            let password = Zeroizing::new(password.to_owned());
            crate::offload::offload_blocking(move || {
                primitives::hash_password_with(&password, scheme)
            })
            .await
        }
        #[cfg(target_arch = "wasm32")]
        {
            primitives::hash_password_with(password, scheme)
        }
    }

    /// Verify against whichever scheme the **stored hash** names, not the
    /// one this service is configured to write.
    ///
    /// A stored hash records how it was written; the configured scheme says
    /// what to write next. Checking a credential against the current setting
    /// rather than against itself would make
    /// [`Argon2JwtCryptoService::with_password_scheme`] — or running one
    /// database against two targets that hash differently, which is the
    /// reason the selector exists — silently invalidate every password
    /// already stored.
    ///
    /// Runs where [`CryptoService::hash`] does: on a dedicated thread on a
    /// native host, inline on wasm32.
    async fn compare_hash(&self, password: &str, hash: &str) -> Result<(), CryptoError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let password = Zeroizing::new(password.to_owned());
            let hash = hash.to_owned();
            crate::offload::offload_blocking(move || {
                primitives::verify_password_any_scheme(&password, &hash)
            })
            .await
        }
        #[cfg(target_arch = "wasm32")]
        {
            primitives::verify_password_any_scheme(password, hash)
        }
    }

    async fn sign_for(
        &self,
        block_id: &str,
        claims: BTreeMap<String, serde_json::Value>,
        expiry: Duration,
    ) -> Result<String, CryptoError> {
        let derived = primitives::derive_block_key(self.jwt_secret.as_bytes(), block_id);
        primitives::jwt_sign(claims, expiry, derived.as_bytes())
    }

    /// Verify with [`JwtExpPolicy::Required`]: [`CryptoService::sign_for`]
    /// always stamps `exp`, so a token without one was not minted by this
    /// service and is rejected rather than treated as never-expiring.
    async fn verify_for(
        &self,
        block_id: &str,
        token: &str,
    ) -> Result<BTreeMap<String, serde_json::Value>, CryptoError> {
        let derived = primitives::derive_block_key(self.jwt_secret.as_bytes(), block_id);
        primitives::jwt_verify(token, derived.as_bytes(), JwtExpPolicy::Required)
    }

    async fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
        primitives::random_bytes(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64-char test secret — long enough to satisfy `MIN_JWT_SECRET_LEN`
    /// without leaning on a real secret format.
    const TEST_SECRET: &str = "test-secret-padded-to-32-bytes-or-more-for-validation-aaaaaaaaaa";

    fn test_service() -> Argon2JwtCryptoService {
        Argon2JwtCryptoService::new(TEST_SECRET.to_string()).expect("test secret is long enough")
    }

    fn test_claims() -> BTreeMap<String, serde_json::Value> {
        let mut m = BTreeMap::new();
        m.insert("sub".to_string(), serde_json::json!("user-1"));
        m
    }

    #[tokio::test]
    async fn sign_and_verify_roundtrip() {
        let svc = test_service();
        let token = svc
            .sign_for("my-org/auth", test_claims(), Duration::from_secs(3600))
            .await
            .unwrap();
        let claims = svc.verify_for("my-org/auth", &token).await.unwrap();
        assert_eq!(claims.get("sub").unwrap(), &serde_json::json!("user-1"));
        assert!(claims.contains_key("exp"), "sign must stamp exp");
    }

    /// The service verifies with `JwtExpPolicy::Required`: its own
    /// `sign_for` always stamps `exp`, so an exp-less token was not minted by
    /// this service and must be rejected rather than treated as
    /// never-expiring.
    #[tokio::test]
    async fn verify_rejects_token_without_exp() {
        use crate::primitives::{b64url_encode, derive_block_key, hmac_sha256};

        // Hand-craft a correctly signed token whose payload has no `exp`.
        let key = derive_block_key(TEST_SECRET.as_bytes(), "my-org/auth");
        let header_b64 = b64url_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload_b64 = b64url_encode(br#"{"sub":"user-1"}"#);
        let signing_input = format!("{header_b64}.{payload_b64}");
        let sig = hmac_sha256(key.as_bytes(), signing_input.as_bytes());
        let token = format!("{signing_input}.{}", b64url_encode(&sig));

        let err = test_service()
            .verify_for("my-org/auth", &token)
            .await
            .expect_err("exp-less token must be rejected");
        assert!(
            err.to_string().contains("missing exp"),
            "expected missing-exp rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn sign_for_different_blocks_produces_different_tokens() {
        let svc = test_service();
        let expiry = Duration::from_secs(3600);

        let token_a = svc
            .sign_for("my-org/auth", test_claims(), expiry)
            .await
            .unwrap();
        let token_b = svc
            .sign_for("my-org/admin", test_claims(), expiry)
            .await
            .unwrap();

        // Tokens signed with different derived keys must differ (the signature
        // portion will be different even though the payload is the same).
        assert_ne!(token_a, token_b);
    }

    #[tokio::test]
    async fn verify_for_correct_block_succeeds() {
        let svc = test_service();
        let expiry = Duration::from_secs(3600);

        let token = svc
            .sign_for("my-org/auth", test_claims(), expiry)
            .await
            .unwrap();
        let claims = svc.verify_for("my-org/auth", &token).await.unwrap();
        assert_eq!(claims.get("sub").unwrap(), &serde_json::json!("user-1"));
    }

    #[tokio::test]
    async fn verify_for_wrong_block_fails() {
        let svc = test_service();
        let expiry = Duration::from_secs(3600);

        let token = svc
            .sign_for("my-org/auth", test_claims(), expiry)
            .await
            .unwrap();
        let result = svc.verify_for("my-org/admin", &token).await;
        assert!(
            result.is_err(),
            "token signed for auth must not verify under admin"
        );
    }

    /// A token signed with the master secret itself is no block's token.
    #[tokio::test]
    async fn a_master_key_token_verifies_for_no_block() {
        let master = crate::primitives::jwt_sign(
            test_claims(),
            Duration::from_secs(3600),
            TEST_SECRET.as_bytes(),
        )
        .unwrap();
        assert!(test_service()
            .verify_for("my-org/auth", &master)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn hash_and_compare_password() {
        let svc = test_service();
        let hash = svc.hash("correcthorsebatterystaple").await.unwrap();
        assert!(hash.starts_with("$argon2id$"));
        svc.compare_hash("correcthorsebatterystaple", &hash)
            .await
            .unwrap();
        assert!(matches!(
            svc.compare_hash("wrong", &hash).await,
            Err(CryptoError::PasswordMismatch)
        ));
    }

    #[test]
    fn new_rejects_short_secret() {
        // 31 bytes — one byte short of MIN_JWT_SECRET_LEN (32).
        let short = "a".repeat(MIN_JWT_SECRET_LEN - 1);
        match Argon2JwtCryptoService::new(short) {
            Ok(_) => panic!("short secret must error"),
            Err(CryptoError::Other(msg)) => assert!(
                msg.contains("at least 32 bytes"),
                "expected length error, got: {msg}"
            ),
            Err(other) => panic!("expected CryptoError::Other, got: {other:?}"),
        }
    }

    #[test]
    fn new_accepts_exactly_min_length_secret() {
        let exact = "a".repeat(MIN_JWT_SECRET_LEN);
        assert!(Argon2JwtCryptoService::new(exact).is_ok());
    }

    #[test]
    fn new_rejects_empty_secret() {
        match Argon2JwtCryptoService::new(String::new()) {
            Ok(_) => panic!("empty secret must error"),
            Err(CryptoError::Other(_)) => {}
            Err(other) => panic!("expected CryptoError::Other, got: {other:?}"),
        }
    }
}

#[cfg(test)]
mod password_scheme_tests {
    use super::*;
    use crate::primitives::PBKDF2_SHA256_MIN_ITERATIONS;

    const TEST_SECRET: &str = "test-secret-padded-to-32-bytes-or-more-for-validation-aaaaaaaaaa";

    /// A PBKDF2 hash written by an independent implementation of the same
    /// scheme (see `primitives::pbkdf2_tests`). It stands in for a
    /// credential already stored by a deployment that hashes with PBKDF2.
    const STORED_PBKDF2: &str =
        "$pbkdf2-sha256$i=1000$AAECAwQFBgcICQoLDA0ODw==$/6tPyT3P0FDTAPcc3qfsdyi1rxNk5iabYJNHAbMJ8Mg=";
    const STORED_PBKDF2_PASSWORD: &str = "correcthorsebatterystaple";

    fn svc() -> Argon2JwtCryptoService {
        Argon2JwtCryptoService::new(TEST_SECRET.to_string()).expect("long enough")
    }

    /// The default is what it always was. A service built the old way keeps
    /// writing argon2id at the default cost, so this change is inert for
    /// every existing caller.
    #[tokio::test]
    async fn the_default_service_still_writes_argon2id() {
        let hash = svc().hash("pw").await.expect("hash");
        assert!(hash.starts_with("$argon2id$"), "{hash}");
    }

    #[tokio::test]
    async fn constrained_argon2_is_selectable() {
        let s = svc().with_password_scheme(PasswordScheme::Argon2(Argon2Cost::Constrained));
        let hash = s.hash("pw").await.expect("hash");
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(
            hash.contains("m=4096"),
            "the constrained memory cost must reach the hash: {hash}"
        );
        s.compare_hash("pw", &hash).await.expect("round trip");
    }

    #[tokio::test]
    async fn pbkdf2_is_selectable() {
        let s = svc().with_password_scheme(PasswordScheme::Pbkdf2Sha256 {
            iterations: PBKDF2_SHA256_MIN_ITERATIONS,
        });
        let hash = s.hash("pw").await.expect("hash");
        assert!(hash.starts_with("$pbkdf2-sha256$i=10000$"), "{hash}");
        s.compare_hash("pw", &hash).await.expect("round trip");
        assert!(matches!(
            s.compare_hash("wrong", &hash).await,
            Err(CryptoError::PasswordMismatch)
        ));
    }

    /// The property the whole design turns on: the configured scheme decides
    /// what a *new* hash looks like and nothing else. Credentials already
    /// stored under the other scheme keep verifying, in both directions —
    /// otherwise selecting a scheme would be a password reset for every
    /// existing user.
    #[tokio::test]
    async fn switching_scheme_does_not_invalidate_stored_credentials() {
        let argon2_svc = svc();
        let stored_argon2 = argon2_svc.hash("pw-argon2").await.expect("hash");

        let pbkdf2_svc = svc().with_password_scheme(PasswordScheme::Pbkdf2Sha256 {
            iterations: PBKDF2_SHA256_MIN_ITERATIONS,
        });

        // A service now writing PBKDF2 still reads argon2 credentials…
        pbkdf2_svc
            .compare_hash("pw-argon2", &stored_argon2)
            .await
            .expect("argon2 credential survives the switch to pbkdf2");

        // …and a service writing argon2 still reads PBKDF2 credentials,
        // including one it did not produce and whose cost is below the
        // floor it would write today.
        argon2_svc
            .compare_hash(STORED_PBKDF2_PASSWORD, STORED_PBKDF2)
            .await
            .expect("pbkdf2 credential survives the switch to argon2");

        // Wrong passwords stay wrong across both.
        assert!(matches!(
            pbkdf2_svc.compare_hash("nope", &stored_argon2).await,
            Err(CryptoError::PasswordMismatch)
        ));
        assert!(matches!(
            argon2_svc.compare_hash("nope", STORED_PBKDF2).await,
            Err(CryptoError::PasswordMismatch)
        ));
    }

    /// A scheme selection cannot be talked into writing a weak hash.
    #[tokio::test]
    async fn a_below_floor_iteration_count_fails_at_hash_time() {
        let s = svc().with_password_scheme(PasswordScheme::Pbkdf2Sha256 { iterations: 1 });
        assert!(matches!(s.hash("pw").await, Err(CryptoError::HashError(_))));
    }

    /// The scheme is about passwords only; JWT signing, per-block key
    /// derivation and randomness are untouched by it.
    #[tokio::test]
    async fn the_scheme_does_not_affect_tokens() {
        let plain = svc();
        let scheme = svc().with_password_scheme(PasswordScheme::Pbkdf2Sha256 {
            iterations: PBKDF2_SHA256_MIN_ITERATIONS,
        });

        let mut claims = BTreeMap::new();
        claims.insert("sub".to_string(), serde_json::json!("user-1"));

        let token = plain
            .sign_for("my-org/auth", claims.clone(), Duration::from_secs(3600))
            .await
            .expect("sign");
        let back = scheme
            .verify_for("my-org/auth", &token)
            .await
            .expect("a token signed by either verifies in both");
        assert_eq!(back.get("sub"), Some(&serde_json::json!("user-1")));

        let block_token = scheme
            .sign_for("my-org/auth", claims, Duration::from_secs(3600))
            .await
            .expect("sign_for");
        plain
            .verify_for("my-org/auth", &block_token)
            .await
            .expect("per-block derivation is unchanged");
    }
}

/// Where password work runs on a native host, driven by `futures`' own
/// executor rather than Tokio's: the service must not need a particular
/// runtime, and Argon2 must not run on the thread that polls the future.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod offload_tests {
    use std::{
        cell::Cell,
        future::Future,
        pin::Pin,
        task::{Context, Poll},
    };

    use super::*;

    const TEST_SECRET: &str = "test-secret-padded-to-32-bytes-or-more-for-validation-aaaaaaaaaa";

    fn svc() -> Argon2JwtCryptoService {
        Argon2JwtCryptoService::new(TEST_SECRET.to_string()).expect("long enough")
    }

    /// Returns `Pending` once, waking itself: lets the executor poll the
    /// other futures joined with it.
    struct YieldNow(bool);

    impl Future for YieldNow {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                return Poll::Ready(());
            }
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    /// No Tokio runtime anywhere: hashing and verifying still work.
    #[test]
    fn password_ops_work_without_a_tokio_runtime() {
        let svc = svc();
        futures::executor::block_on(async {
            let hash = svc.hash("correct horse").await.expect("hash");
            svc.compare_hash("correct horse", &hash)
                .await
                .expect("the right password verifies");
            assert!(matches!(
                svc.compare_hash("wrong", &hash).await,
                Err(CryptoError::PasswordMismatch)
            ));
        });
    }

    /// PERF-02: while a hash runs, the thread polling it keeps running other
    /// work. A derivation run inline finishes inside its first poll, before
    /// the ticker joined with it is ever polled, and the ticker counts
    /// nothing.
    #[test]
    fn argon2_runs_off_the_polling_thread() {
        let svc = svc();
        let done = Cell::new(false);
        let ticks = Cell::new(0_u64);
        futures::executor::block_on(async {
            let hash = async {
                let hash = svc.hash("correct horse").await;
                done.set(true);
                hash
            };
            let ticker = async {
                while !done.get() {
                    ticks.set(ticks.get() + 1);
                    YieldNow(false).await;
                }
            };
            let (hash, ()) = futures::join!(hash, ticker);
            hash.expect("hash");
        });
        assert!(
            ticks.get() > 0,
            "the polling thread did no other work while Argon2 ran"
        );
    }
}
