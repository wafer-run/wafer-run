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
        // A stored credential that cannot be checked is a server-side fault,
        // never a wrong password: `Unauthenticated` here would tell the caller
        // (and its logs) that the user mistyped.
        e @ CryptoError::MalformedHash(_) => WaferError::new(ErrorCode::Internal, e.to_string()),
        CryptoError::SignError(msg) => WaferError::new(ErrorCode::Internal, msg),
        CryptoError::VerifyError(msg) => WaferError::new(ErrorCode::Unauthenticated, msg),
        CryptoError::Other(msg) => WaferError::new(ErrorCode::Internal, msg),
    }
}

/// Tokens are keyed per calling block, so a token op without one has no key.
fn no_caller_key(op: &str) -> WaferError {
    WaferError::new(
        ErrorCode::PermissionDenied,
        format!("{op} needs a calling block: tokens are signed under the caller's derived key"),
    )
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
/// JWT sign/verify always use the per-block key derived for `caller_id`
/// (the runtime provides this from the calling block's identity). This is a
/// separate concern from WRAP enforcement above — `caller_id` selects the
/// derived key, it does not gate access. A `sign`/`verify` with no
/// `caller_id` is refused with `PermissionDenied`: there is no key to use.
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
                    (
                        "hash".to_string(),
                        ResourceType::Crypto,
                        ResourceAccess::Read,
                    )
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
                |_r| {
                    (
                        "compare_hash".to_string(),
                        ResourceType::Crypto,
                        ResourceAccess::Read,
                    )
                },
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
                    (
                        "sign".to_string(),
                        ResourceType::Crypto,
                        ResourceAccess::Read,
                    )
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            let Some(id) = caller_id else {
                return OutputStream::error(no_caller_key("crypto.sign"));
            };
            match service.sign_for(id, req.claims, Duration::from_secs(req.expiry_secs)) {
                Ok(token) => to_output(&wire::SignResponse { token }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_VERIFY => {
            let req = match decode_and_authorize::<wire::VerifyRequest>(
                ctx,
                body,
                "crypto.verify",
                |_r| {
                    (
                        "verify".to_string(),
                        ResourceType::Crypto,
                        ResourceAccess::Read,
                    )
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let Some(id) = caller_id else {
                return OutputStream::error(no_caller_key("crypto.verify"));
            };
            match service.verify_for(id, &req.token) {
                Ok(claims) => to_output(&wire::VerifyResponse { claims }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_RANDOM_BYTES => {
            let req = match decode_and_authorize::<wire::RandomBytesRequest>(
                ctx,
                body,
                "crypto.random_bytes",
                |_r| {
                    (
                        "random_bytes".to_string(),
                        ResourceType::Crypto,
                        ResourceAccess::Read,
                    )
                },
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
fn argon2_permits() -> &'static std::sync::Arc<tokio::sync::Semaphore> {
    static PERMITS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    PERMITS.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        std::sync::Arc::new(tokio::sync::Semaphore::new((cores / 2).clamp(1, 4)))
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
    offload_blocking_on(std::sync::Arc::clone(argon2_permits()), f).await
}

/// Run `f` on the blocking pool while holding one of `permits`.
///
/// The permit moves into the blocking job and is released when `f` returns,
/// not when the caller stops waiting: a blocking job cannot be cancelled, so
/// a caller that drops this future (a client disconnect) leaves the job
/// running, and the job still counts against the cap.
#[cfg(not(target_arch = "wasm32"))]
async fn offload_blocking_on<T, F>(
    permits: std::sync::Arc<tokio::sync::Semaphore>,
    f: F,
) -> Result<T, CryptoError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CryptoError> + Send + 'static,
{
    let permit = permits
        .acquire_owned()
        .await
        .map_err(|e| CryptoError::Other(format!("crypto offload semaphore closed: {e}")))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
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
                    (
                        "hash".to_string(),
                        ResourceType::Crypto,
                        ResourceAccess::Read,
                    )
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
                |_r| {
                    (
                        "compare_hash".to_string(),
                        ResourceType::Crypto,
                        ResourceAccess::Read,
                    )
                },
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod offload_tests {
    use std::{sync::Arc, time::Duration};

    use tokio::sync::Semaphore;

    use super::offload_blocking_on;

    /// A caller that stops waiting (a dropped request future) must not hand
    /// its permit back while its blocking job is still running — otherwise
    /// every disconnect starts another uncapped Argon2 job.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cancelled_caller_keeps_its_permit_until_the_job_ends() {
        let permits = Arc::new(Semaphore::new(2));
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

        let caller = tokio::spawn(offload_blocking_on(Arc::clone(&permits), move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            done_tx.send(()).unwrap();
            Ok(())
        }));

        tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(10)))
            .await
            .unwrap()
            .expect("the blocking job must start");
        assert_eq!(permits.available_permits(), 1);

        // The caller goes away; the blocking job cannot and does not.
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert_eq!(
            permits.available_permits(),
            1,
            "the running job must still hold its permit after its caller was dropped"
        );

        release_tx.send(()).unwrap();
        tokio::task::spawn_blocking(move || done_rx.recv_timeout(Duration::from_secs(10)))
            .await
            .unwrap()
            .expect("the blocking job must finish");
        // The permit drops as the closure returns; give the pool thread a
        // moment to unwind past it.
        for _ in 0..100 {
            if permits.available_permits() == 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the permit must come back once the job ends");
    }
}
