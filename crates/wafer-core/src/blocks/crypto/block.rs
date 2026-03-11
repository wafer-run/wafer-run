use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use wafer_run::block::{Block, BlockInfo};
use wafer_run::common::{ErrorCode, ServiceOp};
use wafer_run::context::Context;
use super::service::{CryptoError, CryptoService};
use wafer_run::types::*;
use wafer_run::helpers::respond_json;

/// CryptoBlock wraps a CryptoService and exposes it as a Block.
///
/// The service is initialized during `lifecycle(Init)` from config
/// (reads `JWT_SECRET` env var or `jwt_secret` config key).
pub struct CryptoBlock {
    service: OnceLock<Arc<dyn CryptoService>>,
}

impl CryptoBlock {
    pub fn new() -> Self {
        Self { service: OnceLock::new() }
    }

    fn svc(&self) -> &dyn CryptoService {
        self.service
            .get()
            .expect("@wafer/crypto: not initialized — call lifecycle(Init) first")
            .as_ref()
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

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
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

    async fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
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
                match self.svc().hash(&req.password) {
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
                match self.svc().compare_hash(&req.password, &req.hash) {
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
                match self.svc().sign(req.claims, expiry) {
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
                match self.svc().verify(&req.token) {
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
                match self.svc().random_bytes(req.n) {
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

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init && self.service.get().is_none() {
            let config: Option<serde_json::Value> = if !event.data.is_empty() {
                serde_json::from_slice(&event.data).ok()
            } else {
                None
            };

            let jwt_secret = crate::blocks::env_or_config_str(
                "JWT_SECRET",
                config.as_ref(),
                "jwt_secret",
            )
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                let mode = std::env::var("WAFER_ENV").unwrap_or_default();
                if mode == "production" || mode == "prod" {
                    tracing::error!(
                        "JWT_SECRET not set in production — set JWT_SECRET environment variable. \
                         Using auto-generated secret; tokens will NOT survive restarts."
                    );
                } else {
                    tracing::warn!(
                        "JWT_SECRET not set — generating a random secret (tokens will not survive restarts)"
                    );
                }
                format!(
                    "wafer-auto-{}{}",
                    uuid::Uuid::new_v4().as_simple(),
                    uuid::Uuid::new_v4().as_simple(),
                )
            });

            let svc = super::service::Argon2JwtCryptoService::new(jwt_secret);
            self.service.set(Arc::new(svc)).ok();
        }
        Ok(())
    }
}
