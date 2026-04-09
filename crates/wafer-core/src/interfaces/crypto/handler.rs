//! Shared message handler logic for the crypto block.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use wafer_block::common::{ErrorCode, ServiceOp};
use wafer_block::helpers::respond_json;
use wafer_block::*;

use super::service::{CryptoError, CryptoService};

// --- Request types ---

#[derive(Deserialize)]
struct HashRequest {
    password: String,
}

#[derive(Deserialize)]
struct CompareHashRequest {
    password: String,
    hash: String,
}

#[derive(Deserialize)]
struct SignRequest {
    claims: HashMap<String, serde_json::Value>,
    #[serde(default = "default_expiry")]
    expiry_secs: u64,
}

fn default_expiry() -> u64 {
    3600
}

#[derive(Deserialize)]
struct VerifyRequest {
    token: String,
}

#[derive(Deserialize)]
struct RandomBytesRequest {
    #[serde(default = "default_random_len")]
    n: usize,
}

fn default_random_len() -> usize {
    32
}

// --- Response types ---

#[derive(Serialize)]
struct HashResponse {
    hash: String,
}

#[derive(Serialize)]
struct CompareHashResponse {
    #[serde(rename = "match")]
    matches: bool,
}

#[derive(Serialize)]
struct SignResponse {
    token: String,
}

#[derive(Serialize)]
struct VerifyResponse {
    claims: HashMap<String, serde_json::Value>,
}

#[derive(Serialize)]
struct RandomBytesResponse {
    bytes: Vec<u8>,
}

// --- Helpers ---

fn crypto_error_to_wafer(e: CryptoError) -> WaferError {
    match e {
        CryptoError::HashError(msg) => WaferError::new(ErrorCode::INTERNAL, msg),
        CryptoError::PasswordMismatch => {
            WaferError::new(ErrorCode::UNAUTHENTICATED, "password mismatch")
        }
        CryptoError::SignError(msg) => WaferError::new(ErrorCode::INTERNAL, msg),
        CryptoError::VerifyError(msg) => WaferError::new(ErrorCode::UNAUTHENTICATED, msg),
        CryptoError::Other(msg) => WaferError::new(ErrorCode::INTERNAL, msg),
    }
}

/// Meta key to opt in to per-block HKDF key derivation.
/// Set to the block ID (e.g. `"suppers-ai/auth"`) to derive a per-block key.
/// When absent, the master key is used.
const META_CRYPTO_BLOCK_KEY: &str = "crypto.block_key";

/// Handle a crypto message by delegating to the given service.
///
/// By default, JWT sign/verify use the master key. To opt in to per-block
/// derived keys, set `crypto.block_key` meta on the message to the block ID.
pub fn handle_message(
    service: &dyn CryptoService,
    _caller_id: Option<&str>,
    msg: &mut Message,
) -> Result_ {
    match msg.kind.as_str() {
        ServiceOp::CRYPTO_HASH => {
            let req: HashRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid crypto.hash request: {e}"),
                    ))
                }
            };
            match service.hash(&req.password) {
                Ok(hash) => respond_json(msg, &HashResponse { hash }),
                Err(e) => Result_::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_COMPARE_HASH => {
            let req: CompareHashRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid crypto.compare_hash request: {e}"),
                    ))
                }
            };
            match service.compare_hash(&req.password, &req.hash) {
                Ok(()) => respond_json(msg, &CompareHashResponse { matches: true }),
                Err(CryptoError::PasswordMismatch) => {
                    respond_json(msg, &CompareHashResponse { matches: false })
                }
                Err(e) => Result_::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_SIGN => {
            let req: SignRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid crypto.sign request: {e}"),
                    ))
                }
            };
            let expiry = Duration::from_secs(req.expiry_secs);
            let block_key = msg.get_meta(META_CRYPTO_BLOCK_KEY);
            let result = if block_key.is_empty() {
                service.sign(req.claims, expiry)
            } else {
                service.sign_for(block_key, req.claims, expiry)
            };
            match result {
                Ok(token) => respond_json(msg, &SignResponse { token }),
                Err(e) => Result_::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_VERIFY => {
            let req: VerifyRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid crypto.verify request: {e}"),
                    ))
                }
            };
            let block_key = msg.get_meta(META_CRYPTO_BLOCK_KEY);
            let result = if block_key.is_empty() {
                service.verify(&req.token)
            } else {
                service.verify_for(block_key, &req.token)
            };
            match result {
                Ok(claims) => respond_json(msg, &VerifyResponse { claims }),
                Err(e) => Result_::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_RANDOM_BYTES => {
            let req: RandomBytesRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid crypto.random_bytes request: {e}"),
                    ))
                }
            };
            const MAX_RANDOM_BYTES: usize = 1_048_576;
            if req.n > MAX_RANDOM_BYTES {
                return Result_::error(WaferError::new(
                    ErrorCode::INVALID_ARGUMENT,
                    format!(
                        "random_bytes n={} exceeds maximum of {}",
                        req.n, MAX_RANDOM_BYTES
                    ),
                ));
            }
            match service.random_bytes(req.n) {
                Ok(bytes) => respond_json(msg, &RandomBytesResponse { bytes }),
                Err(e) => Result_::error(crypto_error_to_wafer(e)),
            }
        }
        other => Result_::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown crypto operation: {other}"),
        )),
    }
}
