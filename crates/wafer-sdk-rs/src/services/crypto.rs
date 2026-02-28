//! Crypto service client using WIT-generated imports.

use std::collections::HashMap;

use crate::wafer::block_world::crypto as wit;

/// Crypto error type.
#[derive(Debug, Clone)]
pub struct CryptoError {
    pub kind: String,
    pub message: String,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for CryptoError {}

fn convert_wit_error(e: wit::CryptoError) -> CryptoError {
    match e {
        wit::CryptoError::HashError => CryptoError { kind: "hash_error".into(), message: "hash operation failed".into() },
        wit::CryptoError::PasswordMismatch => CryptoError { kind: "password_mismatch".into(), message: "password does not match".into() },
        wit::CryptoError::SignError => CryptoError { kind: "sign_error".into(), message: "sign operation failed".into() },
        wit::CryptoError::VerifyError => CryptoError { kind: "verify_error".into(), message: "verify operation failed".into() },
        wit::CryptoError::Other => CryptoError { kind: "other".into(), message: "crypto error".into() },
    }
}

/// Produce a one-way hash of a password.
pub fn hash(password: &str) -> Result<String, CryptoError> {
    wit::hash(password).map_err(convert_wit_error)
}

/// Check a password against a hash. Returns Ok(()) if match.
pub fn compare_hash(password: &str, hash: &str) -> Result<(), CryptoError> {
    wit::compare_hash(password, hash).map_err(convert_wit_error)
}

/// Create a signed token from claims with the given expiry in seconds.
pub fn sign(claims: &HashMap<String, serde_json::Value>, expiry_secs: u64) -> Result<String, CryptoError> {
    let json = serde_json::to_string(claims).unwrap_or_default();
    wit::sign(&json, expiry_secs).map_err(convert_wit_error)
}

/// Verify a token and return its claims.
pub fn verify(token: &str) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
    wit::verify(token)
        .map(|json| serde_json::from_str(&json).unwrap_or_default())
        .map_err(convert_wit_error)
}

/// Generate n cryptographically-secure random bytes.
pub fn random_bytes(n: u32) -> Result<Vec<u8>, CryptoError> {
    wit::random_bytes(n).map_err(convert_wit_error)
}
