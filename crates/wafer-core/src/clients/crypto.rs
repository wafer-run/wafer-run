use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use wafer_run::common::{ErrorCode, ServiceOp};
use wafer_run::context::Context;
use wafer_run::types::WaferError;

use super::{call_service, decode};

const BLOCK: &str = "@wafer/crypto";

// --- Wire-format types ---

#[derive(Serialize)]
struct HashReq<'a> {
    password: &'a str,
}

#[derive(Deserialize)]
struct HashResp {
    hash: String,
}

#[derive(Serialize)]
struct CompareHashReq<'a> {
    password: &'a str,
    hash: &'a str,
}

#[derive(Deserialize)]
struct CompareHashResp {
    #[serde(rename = "match")]
    matches: bool,
}

#[derive(Serialize)]
struct SignReq<'a> {
    claims: &'a HashMap<String, serde_json::Value>,
    expiry_secs: u64,
}

#[derive(Deserialize)]
struct SignResp {
    token: String,
}

#[derive(Serialize)]
struct VerifyReq<'a> {
    token: &'a str,
}

#[derive(Deserialize)]
struct VerifyResp {
    claims: HashMap<String, serde_json::Value>,
}

#[derive(Serialize)]
struct RandomBytesReq {
    n: usize,
}

#[derive(Deserialize)]
struct RandomBytesResp {
    bytes: Vec<u8>,
}

// --- Public API ---

/// Hash a password (Argon2).
pub fn hash(ctx: &dyn Context, password: &str) -> Result<String, WaferError> {
    let data = call_service(ctx, BLOCK, ServiceOp::CRYPTO_HASH, &HashReq { password })?;
    let resp: HashResp = decode(&data)?;
    Ok(resp.hash)
}

/// Compare a password against a hash.
/// Returns `Ok(())` on match, `Err` with `UNAUTHENTICATED` on mismatch.
pub fn compare_hash(ctx: &dyn Context, password: &str, hash: &str) -> Result<(), WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::CRYPTO_COMPARE_HASH,
        &CompareHashReq { password, hash },
    )?;
    let resp: CompareHashResp = decode(&data)?;
    if resp.matches {
        Ok(())
    } else {
        Err(WaferError::new(
            ErrorCode::UNAUTHENTICATED,
            "password mismatch",
        ))
    }
}

/// Sign claims into a JWT with the given expiry.
pub fn sign(
    ctx: &dyn Context,
    claims: &HashMap<String, serde_json::Value>,
    expiry: std::time::Duration,
) -> Result<String, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::CRYPTO_SIGN,
        &SignReq {
            claims,
            expiry_secs: expiry.as_secs(),
        },
    )?;
    let resp: SignResp = decode(&data)?;
    Ok(resp.token)
}

/// Verify a JWT and return its claims.
pub fn verify(
    ctx: &dyn Context,
    token: &str,
) -> Result<HashMap<String, serde_json::Value>, WaferError> {
    let data = call_service(ctx, BLOCK, ServiceOp::CRYPTO_VERIFY, &VerifyReq { token })?;
    let resp: VerifyResp = decode(&data)?;
    Ok(resp.claims)
}

/// Generate `n` cryptographically-secure random bytes.
pub fn random_bytes(ctx: &dyn Context, n: usize) -> Result<Vec<u8>, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::CRYPTO_RANDOM_BYTES,
        &RandomBytesReq { n },
    )?;
    let resp: RandomBytesResp = decode(&data)?;
    Ok(resp.bytes)
}
