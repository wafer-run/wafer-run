//! Shared message handler logic for the crypto block.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use wafer_block::common::{ErrorCode, ServiceOp};
use wafer_block::streams::output::OutputStream;
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

use crate::interfaces::handler_util::{decode_or_err, to_output};

/// Handle a crypto message by delegating to the given service.
///
/// JWT sign/verify use per-block HKDF-derived keys when `caller_id` is set
/// (the runtime provides this from the calling block's identity).
/// When `caller_id` is None, the master key is used.
pub fn handle_message(
    service: &dyn CryptoService,
    caller_id: Option<&str>,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::CRYPTO_HASH => {
            let req = decode_or_err!(body, HashRequest, "crypto.hash");
            match service.hash(&req.password) {
                Ok(hash) => to_output(&HashResponse { hash }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_COMPARE_HASH => {
            let req = decode_or_err!(body, CompareHashRequest, "crypto.compare_hash");
            match service.compare_hash(&req.password, &req.hash) {
                Ok(()) => to_output(&CompareHashResponse { matches: true }),
                Err(CryptoError::PasswordMismatch) => {
                    to_output(&CompareHashResponse { matches: false })
                }
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_SIGN => {
            let req = decode_or_err!(body, SignRequest, "crypto.sign");
            let expiry = Duration::from_secs(req.expiry_secs);
            let result = match caller_id {
                Some(id) => service.sign_for(id, req.claims, expiry),
                None => service.sign(req.claims, expiry),
            };
            match result {
                Ok(token) => to_output(&SignResponse { token }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_VERIFY => {
            let req = decode_or_err!(body, VerifyRequest, "crypto.verify");
            let result = match caller_id {
                Some(id) => service.verify_for(id, &req.token),
                None => service.verify(&req.token),
            };
            match result {
                Ok(claims) => to_output(&VerifyResponse { claims }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_RANDOM_BYTES => {
            let req = decode_or_err!(body, RandomBytesRequest, "crypto.random_bytes");
            const MAX_RANDOM_BYTES: usize = 1_048_576;
            if req.n > MAX_RANDOM_BYTES {
                return OutputStream::error(WaferError::new(
                    ErrorCode::INVALID_ARGUMENT,
                    format!(
                        "random_bytes n={} exceeds maximum of {}",
                        req.n, MAX_RANDOM_BYTES
                    ),
                ));
            }
            match service.random_bytes(req.n) {
                Ok(bytes) => to_output(&RandomBytesResponse { bytes }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown crypto operation: {other}"),
        )),
    }
}
