use std::collections::HashMap;
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CryptoError {
    #[error("hash error: {0}")]
    HashError(String),
    #[error("password mismatch")]
    PasswordMismatch,
    #[error("sign error: {0}")]
    SignError(String),
    #[error("verify error: {0}")]
    VerifyError(String),
    #[error("{0}")]
    Other(String),
}

/// Service provides cryptographic operations.
pub trait CryptoService: Send + Sync {
    /// Hash produces a one-way hash of a password.
    fn hash(&self, password: &str) -> Result<String, CryptoError>;

    /// CompareHash checks a password against a hash.
    fn compare_hash(&self, password: &str, hash: &str) -> Result<(), CryptoError>;

    /// Sign creates a signed token from claims with the given expiry.
    fn sign(
        &self,
        claims: HashMap<String, serde_json::Value>,
        expiry: Duration,
    ) -> Result<String, CryptoError>;

    /// Verify validates a token and returns its claims.
    fn verify(&self, token: &str) -> Result<HashMap<String, serde_json::Value>, CryptoError>;

    /// RandomBytes generates n cryptographically-secure random bytes.
    fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError>;
}

// ---------------------------------------------------------------------------
// Argon2 + JWT concrete implementation
// ---------------------------------------------------------------------------

/// Argon2 + JWT crypto service.
///
/// Password hashing uses argon2id with default parameters.
/// Token signing uses HMAC-SHA256 via the `jsonwebtoken` crate.
#[cfg(feature = "crypto")]
pub struct Argon2JwtCryptoService {
    jwt_secret: String,
}

#[cfg(feature = "crypto")]
impl Argon2JwtCryptoService {
    pub fn new(jwt_secret: String) -> Self {
        Self { jwt_secret }
    }
}

#[cfg(feature = "crypto")]
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
        let parsed = PasswordHash::new(hash)
            .map_err(|e| CryptoError::HashError(e.to_string()))?;
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

    fn verify(
        &self,
        token: &str,
    ) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
        use jsonwebtoken::{decode, DecodingKey, Validation};

        let key = DecodingKey::from_secret(self.jwt_secret.as_bytes());
        let validation = Validation::default();

        let data = decode::<HashMap<String, serde_json::Value>>(token, &key, &validation)
            .map_err(|e| CryptoError::VerifyError(e.to_string()))?;

        Ok(data.claims)
    }

    fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
        use argon2::password_hash::rand_core::{OsRng, RngCore};
        let mut buf = vec![0u8; n];
        OsRng.fill_bytes(&mut buf);
        Ok(buf)
    }
}
