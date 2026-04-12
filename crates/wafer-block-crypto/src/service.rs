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
/// Token signing uses HMAC-SHA256 (HS256) implemented manually with the
/// `hmac`, `sha2`, and `base64ct` crates — all pure Rust, wasm32-compatible.
pub struct Argon2JwtCryptoService {
    jwt_secret: String,
}

impl Argon2JwtCryptoService {
    pub fn new(jwt_secret: String) -> Self {
        Self { jwt_secret }
    }
}

// ---------------------------------------------------------------------------
// Pure-Rust HS256 JWT helpers
// ---------------------------------------------------------------------------

/// Standard JWT header for HS256, base64url-encoded (no padding).
/// {"alg":"HS256","typ":"JWT"}
const JWT_HEADER_B64: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";

/// Encode bytes as base64url without padding (as required by JWT RFC 7515).
fn b64url_encode(data: &[u8]) -> String {
    use base64ct::{Base64UrlUnpadded, Encoding};
    Base64UrlUnpadded::encode_string(data)
}

/// Decode a base64url (no-padding) string into bytes.
fn b64url_decode(s: &str) -> Result<Vec<u8>, CryptoError> {
    use base64ct::{Base64UrlUnpadded, Encoding};
    Base64UrlUnpadded::decode_vec(s)
        .map_err(|e| CryptoError::VerifyError(format!("base64 decode: {e}")))
}

/// Compute HMAC-SHA256 over `data` using `key`.
fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Sign a claims map as a compact JWT (HS256).
fn jwt_sign(
    claims: &HashMap<String, serde_json::Value>,
    secret: &[u8],
) -> Result<String, CryptoError> {
    let payload_json =
        serde_json::to_string(claims).map_err(|e| CryptoError::SignError(e.to_string()))?;
    let payload_b64 = b64url_encode(payload_json.as_bytes());

    let signing_input = format!("{JWT_HEADER_B64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = b64url_encode(&sig);

    Ok(format!("{signing_input}.{sig_b64}"))
}

/// Verify a compact JWT (HS256) and return the claims.
///
/// Validates:
/// - Three-part structure
/// - Header matches expected HS256/JWT header
/// - Signature is correct (constant-time comparison via HMAC verify)
/// - `exp` claim has not passed
fn jwt_verify(
    token: &str,
    secret: &[u8],
) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let parts: Vec<&str> = token.splitn(3, '.').collect();
    if parts.len() != 3 {
        return Err(CryptoError::VerifyError(
            "invalid JWT structure".to_string(),
        ));
    }
    let (header_b64, payload_b64, sig_b64) = (parts[0], parts[1], parts[2]);

    // Verify header (we only support HS256 tokens produced by this service).
    if header_b64 != JWT_HEADER_B64 {
        // Fall back: decode and check alg field to allow minor whitespace variants.
        let header_bytes = b64url_decode(header_b64)?;
        let header: serde_json::Value = serde_json::from_slice(&header_bytes)
            .map_err(|e| CryptoError::VerifyError(format!("header decode: {e}")))?;
        let alg = header.get("alg").and_then(|v| v.as_str()).unwrap_or("");
        if alg != "HS256" {
            return Err(CryptoError::VerifyError(format!(
                "unsupported algorithm: {alg}"
            )));
        }
    }

    // Verify signature (constant-time via hmac::Mac::verify_slice).
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig_bytes = b64url_decode(sig_b64)?;
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(signing_input.as_bytes());
    mac.verify_slice(&sig_bytes)
        .map_err(|_| CryptoError::VerifyError("signature mismatch".to_string()))?;

    // Decode payload.
    let payload_bytes = b64url_decode(payload_b64)?;
    let claims: HashMap<String, serde_json::Value> = serde_json::from_slice(&payload_bytes)
        .map_err(|e| CryptoError::VerifyError(format!("payload decode: {e}")))?;

    // Validate expiry.
    let now = chrono::Utc::now().timestamp();
    if let Some(exp) = claims.get("exp").and_then(|v| v.as_i64()) {
        if now > exp {
            return Err(CryptoError::VerifyError("token expired".to_string()));
        }
    }

    Ok(claims)
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
        let now = chrono::Utc::now();
        let exp = now + chrono::Duration::from_std(expiry).unwrap_or(chrono::Duration::hours(1));

        let mut payload = claims;
        payload.insert("iat".to_string(), serde_json::json!(now.timestamp()));
        payload.insert("exp".to_string(), serde_json::json!(exp.timestamp()));

        jwt_sign(&payload, self.jwt_secret.as_bytes())
    }

    fn verify(&self, token: &str) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
        jwt_verify(token, self.jwt_secret.as_bytes())
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
