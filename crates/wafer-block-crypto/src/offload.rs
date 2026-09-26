//! Blocking-pool offload for password hashing on native hosts.
//!
//! Argon2 hashing and verification are CPU-expensive by design, so
//! [`Argon2JwtCryptoService`](crate::service::Argon2JwtCryptoService) runs
//! them on Tokio's blocking pool, behind a small semaphore, instead of on an
//! async executor thread. It therefore needs a Tokio runtime on a native
//! host. wasm32 has no blocking pool and does not compile this module.

use std::sync::{Arc, OnceLock};

use tokio::sync::Semaphore;

use crate::service::CryptoError;

/// Concurrency bound for Argon2 offload jobs: half the cores, clamped to
/// [1, 4]. Each Argon2id hash pins a thread for tens of milliseconds and
/// ~19 MiB of memory, so the cap keeps a burst of auth attempts from
/// saturating the blocking pool (the queue of *waiting* callers is bounded
/// upstream by the server's request/rate limits — waiters here are cheap,
/// cancellable futures, not threads).
fn argon2_permits() -> &'static Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        Arc::new(Semaphore::new((cores / 2).clamp(1, 4)))
    })
}

/// Run a CPU-heavy crypto closure on the blocking pool, bounded by
/// [`argon2_permits`].
pub(crate) async fn offload_blocking<T, F>(f: F) -> Result<T, CryptoError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CryptoError> + Send + 'static,
{
    offload_blocking_on(Arc::clone(argon2_permits()), f).await
}

/// Run `f` on the blocking pool while holding one of `permits`.
///
/// The permit moves into the blocking job and is released when `f` returns,
/// not when the caller stops waiting: a blocking job cannot be cancelled, so
/// a caller that drops this future (a client disconnect) leaves the job
/// running, and the job still counts against the cap.
async fn offload_blocking_on<T, F>(permits: Arc<Semaphore>, f: F) -> Result<T, CryptoError>
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

#[cfg(test)]
mod tests {
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
