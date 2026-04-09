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
pub trait CryptoService: wafer_block::MaybeSend + wafer_block::MaybeSync {
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

    /// Sign creates a signed token using a per-block derived key.
    /// Default falls back to `sign` (ignoring block_id).
    fn sign_for(
        &self,
        _block_id: &str,
        claims: HashMap<String, serde_json::Value>,
        expiry: Duration,
    ) -> Result<String, CryptoError> {
        self.sign(claims, expiry)
    }

    /// Verify validates a token using a per-block derived key.
    /// Default falls back to `verify` (ignoring block_id).
    fn verify_for(
        &self,
        _block_id: &str,
        token: &str,
    ) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
        self.verify(token)
    }

    /// RandomBytes generates n cryptographically-secure random bytes.
    fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError>;
}
