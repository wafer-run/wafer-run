use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use wafer_run::block::{Block, BlockInfo};
use wafer_run::common::{ErrorCode, ServiceOp};
use wafer_run::context::Context;
use super::service::{CryptoError, CryptoService};
use wafer_run::types::*;
use wafer_run::helpers::respond_json;

/// CryptoBlock wraps a CryptoService and exposes it as a Block.
pub struct CryptoBlock {
    service: Arc<dyn CryptoService>,
}

impl CryptoBlock {
    pub fn new(service: Arc<dyn CryptoService>) -> Self {
        Self { service }
    }
}

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
    /// Expiry in seconds.
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
    /// Raw random bytes as a JSON array.
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

impl Block for CryptoBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/crypto".to_string(),
            version: "0.1.0".to_string(),
            interface: "crypto@v1".to_string(),
            summary: "Cryptographic operations via CryptoService".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Native,
            requires: Vec::new(),
        }
    }

    fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
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
                match self.service.hash(&req.password) {
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
                match self.service.compare_hash(&req.password, &req.hash) {
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
                match self.service.sign(req.claims, expiry) {
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
                match self.service.verify(&req.token) {
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
                // Cap at 1 MB to prevent OOM
                const MAX_RANDOM_BYTES: usize = 1_048_576;
                if req.n > MAX_RANDOM_BYTES {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("random_bytes n={} exceeds maximum of {}", req.n, MAX_RANDOM_BYTES),
                    ));
                }
                match self.service.random_bytes(req.n) {
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

    fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}
