use std::{collections::HashMap, time::Duration};

// Re-export the trait and error from wafer-core.
pub use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};

// Re-exported from `primitives` (where the recommendation is documented as
// part of the shared policy) so existing `service::MIN_JWT_SECRET_LEN`
// imports keep working.
pub use crate::primitives::MIN_JWT_SECRET_LEN;
use crate::primitives::{self, Argon2Cost, JwtExpPolicy};

/// Argon2 + JWT crypto service.
///
/// Thin policy wrapper over [`crate::primitives`]: argon2id password
/// hashing at [`Argon2Cost::Default`], HS256 JWT sign/verify with
/// [`JwtExpPolicy::Required`], and per-block keys derived via
/// [`primitives::derive_block_key`]. All pure Rust, wasm32-compatible.
pub struct Argon2JwtCryptoService {
    jwt_secret: String,
}

impl Argon2JwtCryptoService {
    /// Build a service from a JWT signing secret.
    ///
    /// Fails when the secret is shorter than [`MIN_JWT_SECRET_LEN`] bytes
    /// — a weak secret here defeats the security of every token signed
    /// by the runtime, so this is a fail-fast at construction rather
    /// than an issue surfaced per-request.
    pub fn new(jwt_secret: String) -> Result<Self, CryptoError> {
        if jwt_secret.len() < MIN_JWT_SECRET_LEN {
            return Err(CryptoError::Other(format!(
                "JWT secret must be at least {MIN_JWT_SECRET_LEN} bytes (HS256 requires \
                 ≥ HMAC-SHA256 output length per RFC 2104); got {}",
                jwt_secret.len()
            )));
        }
        Ok(Self { jwt_secret })
    }
}

impl CryptoService for Argon2JwtCryptoService {
    fn hash(&self, password: &str) -> Result<String, CryptoError> {
        primitives::hash_password(password, Argon2Cost::Default)
    }

    fn compare_hash(&self, password: &str, hash: &str) -> Result<(), CryptoError> {
        primitives::verify_password(password, hash)
    }

    fn sign(
        &self,
        claims: HashMap<String, serde_json::Value>,
        expiry: Duration,
    ) -> Result<String, CryptoError> {
        primitives::jwt_sign(claims, expiry, self.jwt_secret.as_bytes())
    }

    /// Verify with [`JwtExpPolicy::Required`]: [`CryptoService::sign`]
    /// always stamps `exp`, so a token without one was not minted by this
    /// service and is rejected rather than treated as never-expiring.
    fn verify(&self, token: &str) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
        primitives::jwt_verify(token, self.jwt_secret.as_bytes(), JwtExpPolicy::Required)
    }

    fn sign_for(
        &self,
        block_id: &str,
        claims: HashMap<String, serde_json::Value>,
        expiry: Duration,
    ) -> Result<String, CryptoError> {
        let derived = primitives::derive_block_key(self.jwt_secret.as_bytes(), block_id);
        primitives::jwt_sign(claims, expiry, derived.as_bytes())
    }

    fn verify_for(
        &self,
        block_id: &str,
        token: &str,
    ) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
        let derived = primitives::derive_block_key(self.jwt_secret.as_bytes(), block_id);
        primitives::jwt_verify(token, derived.as_bytes(), JwtExpPolicy::Required)
    }

    fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
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

    fn test_claims() -> HashMap<String, serde_json::Value> {
        let mut m = HashMap::new();
        m.insert("sub".to_string(), serde_json::json!("user-1"));
        m
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let svc = test_service();
        let token = svc.sign(test_claims(), Duration::from_secs(3600)).unwrap();
        let claims = svc.verify(&token).unwrap();
        assert_eq!(claims.get("sub").unwrap(), &serde_json::json!("user-1"));
        assert!(claims.contains_key("exp"), "sign must stamp exp");
    }

    /// The service verifies with `JwtExpPolicy::Required`: its own `sign`
    /// always stamps `exp`, so an exp-less token was not minted by this
    /// service and must be rejected rather than treated as never-expiring.
    /// (This pins the resolution of the historical exp-optional vs
    /// exp-required drift between the runtime and solobase copies.)
    #[test]
    fn verify_rejects_token_without_exp() {
        use crate::primitives::{b64url_encode, hmac_sha256};

        // Hand-craft a correctly signed token whose payload has no `exp`.
        let header_b64 = b64url_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload_b64 = b64url_encode(br#"{"sub":"user-1"}"#);
        let signing_input = format!("{header_b64}.{payload_b64}");
        let sig = hmac_sha256(TEST_SECRET.as_bytes(), signing_input.as_bytes());
        let token = format!("{signing_input}.{}", b64url_encode(&sig));

        let err = test_service()
            .verify(&token)
            .expect_err("exp-less token must be rejected");
        assert!(
            err.to_string().contains("missing exp"),
            "expected missing-exp rejection, got: {err}"
        );
    }

    #[test]
    fn sign_for_different_blocks_produces_different_tokens() {
        let svc = test_service();
        let expiry = Duration::from_secs(3600);

        let token_a = svc
            .sign_for("suppers-ai/auth", test_claims(), expiry)
            .unwrap();
        let token_b = svc
            .sign_for("suppers-ai/admin", test_claims(), expiry)
            .unwrap();

        // Tokens signed with different derived keys must differ (the signature
        // portion will be different even though the payload is the same).
        assert_ne!(token_a, token_b);
    }

    #[test]
    fn verify_for_correct_block_succeeds() {
        let svc = test_service();
        let expiry = Duration::from_secs(3600);

        let token = svc
            .sign_for("suppers-ai/auth", test_claims(), expiry)
            .unwrap();
        let claims = svc.verify_for("suppers-ai/auth", &token).unwrap();
        assert_eq!(claims.get("sub").unwrap(), &serde_json::json!("user-1"));
    }

    #[test]
    fn verify_for_wrong_block_fails() {
        let svc = test_service();
        let expiry = Duration::from_secs(3600);

        let token = svc
            .sign_for("suppers-ai/auth", test_claims(), expiry)
            .unwrap();
        let result = svc.verify_for("suppers-ai/admin", &token);
        assert!(
            result.is_err(),
            "token signed for auth must not verify under admin"
        );
    }

    #[test]
    fn sign_and_sign_for_produce_different_tokens() {
        let svc = test_service();
        let expiry = Duration::from_secs(3600);

        let token_plain = svc.sign(test_claims(), expiry).unwrap();
        let token_block = svc
            .sign_for("suppers-ai/auth", test_claims(), expiry)
            .unwrap();

        assert_ne!(token_plain, token_block);
    }

    #[test]
    fn hash_and_compare_password() {
        let svc = test_service();
        let hash = svc.hash("correcthorsebatterystaple").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        svc.compare_hash("correcthorsebatterystaple", &hash)
            .unwrap();
        assert!(matches!(
            svc.compare_hash("wrong", &hash),
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
