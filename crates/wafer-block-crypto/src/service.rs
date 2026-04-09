use std::collections::HashMap;
use std::time::Duration;

// Re-export the trait and error from wafer-core.
pub use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};

// ---------------------------------------------------------------------------
// Argon2 + JWT concrete implementation
// ---------------------------------------------------------------------------

/// Argon2 + JWT crypto service.
///
/// Password hashing uses argon2id with default parameters.
/// Token signing uses HMAC-SHA256 via the `jsonwebtoken` crate.
pub struct Argon2JwtCryptoService {
    jwt_secret: String,
}

impl Argon2JwtCryptoService {
    pub fn new(jwt_secret: String) -> Self {
        Self { jwt_secret }
    }
}

impl Argon2JwtCryptoService {
    /// Derive a per-block JWT signing key from the master secret using HKDF-SHA256.
    fn derive_block_key(&self, block_id: &str) -> String {
        use hkdf::Hkdf;
        use sha2::Sha256;

        let hk = Hkdf::<Sha256>::new(None, self.jwt_secret.as_bytes());
        let info = format!("wafer-jwt|{block_id}");
        let mut okm = [0u8; 32];
        hk.expand(info.as_bytes(), &mut okm).expect("HKDF expand");
        // Encode as hex for use as JWT secret string
        okm.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl CryptoService for Argon2JwtCryptoService {
    fn hash(&self, password: &str) -> Result<String, CryptoError> {
        use argon2::{
            password_hash::{rand_core::OsRng, SaltString},
            Argon2, PasswordHasher,
        };
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        argon2
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| CryptoError::HashError(e.to_string()))
    }

    fn compare_hash(&self, password: &str, hash: &str) -> Result<(), CryptoError> {
        use argon2::{password_hash::PasswordHash, Argon2, PasswordVerifier};
        let parsed = PasswordHash::new(hash).map_err(|e| CryptoError::HashError(e.to_string()))?;
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .map_err(|_| CryptoError::PasswordMismatch)
    }

    fn sign(
        &self,
        claims: HashMap<String, serde_json::Value>,
        expiry: Duration,
    ) -> Result<String, CryptoError> {
        use jsonwebtoken::{encode, EncodingKey, Header};

        let now = chrono::Utc::now();
        let exp = now + chrono::Duration::from_std(expiry).unwrap_or(chrono::Duration::hours(1));

        let mut payload = claims;
        payload.insert("iat".to_string(), serde_json::json!(now.timestamp()));
        payload.insert("exp".to_string(), serde_json::json!(exp.timestamp()));

        let key = EncodingKey::from_secret(self.jwt_secret.as_bytes());
        encode(&Header::default(), &payload, &key)
            .map_err(|e| CryptoError::SignError(e.to_string()))
    }

    fn verify(&self, token: &str) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
        use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

        let key = DecodingKey::from_secret(self.jwt_secret.as_bytes());
        let validation = Validation::new(Algorithm::HS256);

        let data = decode::<HashMap<String, serde_json::Value>>(token, &key, &validation)
            .map_err(|e| CryptoError::VerifyError(e.to_string()))?;

        Ok(data.claims)
    }

    fn sign_for(
        &self,
        block_id: &str,
        claims: HashMap<String, serde_json::Value>,
        expiry: Duration,
    ) -> Result<String, CryptoError> {
        let derived = self.derive_block_key(block_id);
        let temp = Self::new(derived);
        temp.sign(claims, expiry)
    }

    fn verify_for(
        &self,
        block_id: &str,
        token: &str,
    ) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
        let derived = self.derive_block_key(block_id);
        let temp = Self::new(derived);
        temp.verify(token)
    }

    fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
        use argon2::password_hash::rand_core::{OsRng, RngCore};
        let mut buf = vec![0u8; n];
        OsRng.fill_bytes(&mut buf);
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_service() -> Argon2JwtCryptoService {
        Argon2JwtCryptoService::new("master-secret-for-testing".to_string())
    }

    fn test_claims() -> HashMap<String, serde_json::Value> {
        let mut m = HashMap::new();
        m.insert("sub".to_string(), serde_json::json!("user-1"));
        m
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
    fn derive_block_key_is_deterministic() {
        let svc = test_service();
        let key1 = svc.derive_block_key("suppers-ai/auth");
        let key2 = svc.derive_block_key("suppers-ai/auth");
        assert_eq!(key1, key2);
    }
}
