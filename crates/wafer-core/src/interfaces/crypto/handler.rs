//! Shared message handler logic for the crypto block.

use std::time::Duration;

use wafer_block::{
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    types::ResourceType,
    wire::crypto as wire,
    *,
};

use super::service::{CryptoError, CryptoService};
use crate::interfaces::handler_util::{decode_and_authorize, to_output};

// --- Helpers ---

fn crypto_error_to_wafer(e: CryptoError) -> WaferError {
    match e {
        CryptoError::HashError(msg) => WaferError::new(ErrorCode::Internal, msg),
        CryptoError::PasswordMismatch => {
            WaferError::new(ErrorCode::Unauthenticated, "password mismatch")
        }
        CryptoError::SignError(msg) => WaferError::new(ErrorCode::Internal, msg),
        CryptoError::VerifyError(msg) => WaferError::new(ErrorCode::Unauthenticated, msg),
        CryptoError::Other(msg) => WaferError::new(ErrorCode::Internal, msg),
    }
}

/// Handle a crypto message by delegating to the given service.
///
/// `ctx` is the trusted host-side authorization surface: every op arm
/// authorizes via [`decode_and_authorize`], which bundles the codec decode
/// with a call to `ctx.check_resource_access` so an arm cannot obtain its
/// typed request without also being checked. The resource named is the
/// literal op name (`"hash"`, `"sign"`, ...) — crypto grants are keyed on
/// the operation, not on request content — and `is_write` is always `false`:
/// crypto ops aren't resource writes in the WRAP sense.
///
/// JWT sign/verify use per-block HKDF-derived keys when `caller_id` is set
/// (the runtime provides this from the calling block's identity). This is a
/// separate concern from WRAP enforcement above — `caller_id` selects the
/// derived key, it does not gate access. When `caller_id` is None, the
/// master key is used.
pub fn handle_message(
    service: &dyn CryptoService,
    ctx: &dyn Context,
    caller_id: Option<&str>,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::CRYPTO_HASH => {
            let req =
                match decode_and_authorize::<wire::HashRequest>(ctx, body, "crypto.hash", |_r| {
                    ("hash".to_string(), ResourceType::Crypto, false)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            match service.hash(&req.password) {
                Ok(hash) => to_output(&wire::HashResponse { hash }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_COMPARE_HASH => {
            let req = match decode_and_authorize::<wire::CompareHashRequest>(
                ctx,
                body,
                "crypto.compare_hash",
                |_r| ("compare_hash".to_string(), ResourceType::Crypto, false),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.compare_hash(&req.password, &req.hash) {
                Ok(()) => to_output(&wire::CompareHashResponse { matches: true }),
                Err(CryptoError::PasswordMismatch) => {
                    to_output(&wire::CompareHashResponse { matches: false })
                }
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_SIGN => {
            let req =
                match decode_and_authorize::<wire::SignRequest>(ctx, body, "crypto.sign", |_r| {
                    ("sign".to_string(), ResourceType::Crypto, false)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            let expiry = Duration::from_secs(req.expiry_secs);
            let result = match caller_id {
                Some(id) => service.sign_for(id, req.claims, expiry),
                None => service.sign(req.claims, expiry),
            };
            match result {
                Ok(token) => to_output(&wire::SignResponse { token }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_VERIFY => {
            let req = match decode_and_authorize::<wire::VerifyRequest>(
                ctx,
                body,
                "crypto.verify",
                |_r| ("verify".to_string(), ResourceType::Crypto, false),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let result = match caller_id {
                Some(id) => service.verify_for(id, &req.token),
                None => service.verify(&req.token),
            };
            match result {
                Ok(claims) => to_output(&wire::VerifyResponse { claims }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_RANDOM_BYTES => {
            let req = match decode_and_authorize::<wire::RandomBytesRequest>(
                ctx,
                body,
                "crypto.random_bytes",
                |_r| ("random_bytes".to_string(), ResourceType::Crypto, false),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            const MAX_RANDOM_BYTES: usize = 1_048_576;
            if req.n > MAX_RANDOM_BYTES {
                return OutputStream::error(WaferError::new(
                    ErrorCode::InvalidArgument,
                    format!(
                        "random_bytes n={} exceeds maximum of {}",
                        req.n, MAX_RANDOM_BYTES
                    ),
                ));
            }
            match service.random_bytes(req.n) {
                Ok(bytes) => to_output(&wire::RandomBytesResponse { bytes }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("unknown crypto operation: {other}"),
        )),
    }
}

/// Concurrency bound for Argon2 offload jobs: half the cores, clamped to
/// [1, 4]. Each Argon2id hash pins a thread for tens of milliseconds and
/// ~19 MiB of memory, so the cap keeps a burst of auth attempts from
/// saturating the blocking pool (the queue of *waiting* callers is bounded
/// upstream by the server's request/rate limits — waiters here are cheap,
/// cancellable futures, not threads).
#[cfg(not(target_arch = "wasm32"))]
fn argon2_permits() -> &'static tokio::sync::Semaphore {
    static PERMITS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    PERMITS.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        tokio::sync::Semaphore::new((cores / 2).clamp(1, 4))
    })
}

/// Run a CPU-heavy crypto closure on the blocking pool, bounded by
/// [`argon2_permits`].
#[cfg(not(target_arch = "wasm32"))]
async fn offload_blocking<T, F>(f: F) -> Result<T, CryptoError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CryptoError> + Send + 'static,
{
    let _permit = argon2_permits()
        .acquire()
        .await
        .map_err(|e| CryptoError::Other(format!("crypto offload semaphore closed: {e}")))?;
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CryptoError::Other(format!("crypto blocking task failed: {e}")))?
}

/// Native variant of [`handle_message`]: Argon2 password hashing and
/// verification are CPU-expensive by design, so the `hash` and
/// `compare_hash` ops run on the blocking pool (behind a small semaphore)
/// instead of on an async executor thread (PERF-02). Decode + WRAP
/// authorization happen inline exactly as in the sync path; every other op
/// (cheap HMAC/RNG work) delegates to [`handle_message`] unchanged, as does
/// the wasm32 build, which has no blocking pool.
#[cfg(not(target_arch = "wasm32"))]
pub async fn handle_message_native(
    service: &std::sync::Arc<dyn CryptoService>,
    ctx: &dyn Context,
    caller_id: Option<&str>,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::CRYPTO_HASH => {
            let req =
                match decode_and_authorize::<wire::HashRequest>(ctx, body, "crypto.hash", |_r| {
                    ("hash".to_string(), ResourceType::Crypto, false)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            let svc = std::sync::Arc::clone(service);
            match offload_blocking(move || svc.hash(&req.password)).await {
                Ok(hash) => to_output(&wire::HashResponse { hash }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_COMPARE_HASH => {
            let req = match decode_and_authorize::<wire::CompareHashRequest>(
                ctx,
                body,
                "crypto.compare_hash",
                |_r| ("compare_hash".to_string(), ResourceType::Crypto, false),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let svc = std::sync::Arc::clone(service);
            match offload_blocking(move || svc.compare_hash(&req.password, &req.hash)).await {
                Ok(()) => to_output(&wire::CompareHashResponse { matches: true }),
                Err(CryptoError::PasswordMismatch) => {
                    to_output(&wire::CompareHashResponse { matches: false })
                }
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        _ => handle_message(service.as_ref(), ctx, caller_id, msg, body),
    }
}
