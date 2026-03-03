//! Crypto service client — calls `wafer/crypto` block via `call-block`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::wafer::block_world::runtime;
use crate::wafer::block_world::types::{Action, Message};

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

// --- Internal request/response types ---

#[derive(Serialize)]
struct HashReq<'a> {
    password: &'a str,
}

#[derive(Serialize)]
struct CompareHashReq<'a> {
    password: &'a str,
    hash: &'a str,
}

#[derive(Serialize)]
struct SignReq<'a> {
    claims: &'a str,
    expiry_secs: u64,
}

#[derive(Serialize)]
struct VerifyReq<'a> {
    token: &'a str,
}

#[derive(Serialize)]
struct RandomBytesReq {
    n: u32,
}

#[derive(Deserialize)]
struct StringResp {
    value: String,
}

#[derive(Deserialize)]
struct BytesResp {
    data: Vec<u8>,
}

// --- Helpers ---

fn make_msg(kind: &str, data: &impl Serialize) -> Message {
    Message {
        kind: kind.to_string(),
        data: serde_json::to_vec(data).unwrap_or_default(),
        meta: Vec::new(),
    }
}

fn call_crypto(msg: &Message) -> Result<Vec<u8>, CryptoError> {
    let result = runtime::call_block("wafer/crypto", msg);
    match result.action {
        Action::Error => {
            let err_msg = result.error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown crypto error".to_string());
            let kind = if err_msg.contains("mismatch") {
                "password_mismatch"
            } else if err_msg.contains("hash") {
                "hash_error"
            } else if err_msg.contains("sign") {
                "sign_error"
            } else if err_msg.contains("verify") || err_msg.contains("token") {
                "verify_error"
            } else {
                "other"
            };
            Err(CryptoError { kind: kind.into(), message: err_msg })
        }
        _ => Ok(result.response.map(|r| r.data).unwrap_or_default()),
    }
}

fn call_crypto_parse<T: serde::de::DeserializeOwned>(msg: &Message) -> Result<T, CryptoError> {
    let data = call_crypto(msg)?;
    serde_json::from_slice(&data).map_err(|e| CryptoError {
        kind: "other".into(),
        message: format!("failed to parse response: {e}"),
    })
}

// --- Public API ---

/// Produce a one-way hash of a password.
pub fn hash(password: &str) -> Result<String, CryptoError> {
    let msg = make_msg("crypto.hash", &HashReq { password });
    let resp: StringResp = call_crypto_parse(&msg)?;
    Ok(resp.value)
}

/// Check a password against a hash. Returns Ok(()) if match.
pub fn compare_hash(password: &str, hash: &str) -> Result<(), CryptoError> {
    let msg = make_msg("crypto.compare_hash", &CompareHashReq { password, hash });
    call_crypto(&msg)?;
    Ok(())
}

/// Create a signed token from claims with the given expiry in seconds.
pub fn sign(claims: &HashMap<String, serde_json::Value>, expiry_secs: u64) -> Result<String, CryptoError> {
    let claims_json = serde_json::to_string(claims).unwrap_or_default();
    let msg = make_msg("crypto.sign", &SignReq { claims: &claims_json, expiry_secs });
    let resp: StringResp = call_crypto_parse(&msg)?;
    Ok(resp.value)
}

/// Verify a token and return its claims.
pub fn verify(token: &str) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
    let msg = make_msg("crypto.verify", &VerifyReq { token });
    let data = call_crypto(&msg)?;
    // The response is the claims JSON directly
    serde_json::from_slice(&data).map_err(|e| CryptoError {
        kind: "other".into(),
        message: format!("failed to parse claims: {e}"),
    })
}

/// Generate n cryptographically-secure random bytes.
pub fn random_bytes(n: u32) -> Result<Vec<u8>, CryptoError> {
    let msg = make_msg("crypto.random_bytes", &RandomBytesReq { n });
    let resp: BytesResp = call_crypto_parse(&msg)?;
    Ok(resp.data)
}
