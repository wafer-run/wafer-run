//! Per-block init state container with lazy-once-success semantics.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §3, §4
//!
//! ## Concurrency model — pattern (a): `Mutex<SlotState>`
//!
//! The mutex is held across the entire init call. This means concurrent
//! first-callers for the same block serialize: the second caller waits for
//! the first to finish rather than both running init simultaneously.
//!
//! This is intentional: init is typically I/O (config fetch, DB migration),
//! and running it twice concurrently would either fail or do redundant work.
//! Pattern (b) (`OnceCell`) would let concurrent callers each attempt init
//! independently, which is better for CPU-bound init but wrong for idempotent
//! I/O — running two migrations concurrently on the same table is a bug, not
//! an optimisation.
//!
//! Holding the lock across init is what makes an init cycle between two
//! blocks a potential deadlock; the dispatch path refuses the wait that
//! would close one before it takes this lock (see the crate-private
//! `runtime::init_waits` module).
//!
//! ## Semantics
//! - First `Ok(_)` result is cached for the slot's lifetime.
//! - `Permanent` error is cached for the slot's lifetime.
//! - `Transient` error is NOT cached, but it starts a retry backoff: until
//!   it elapses, callers get the failure back as `Transient` without init
//!   running again. The backoff starts at [`TRANSIENT_RETRY_BASE`], doubles
//!   with each consecutive transient failure up to [`TRANSIENT_RETRY_MAX`],
//!   and a success clears it.
//! - `Cycle` error is NOT cached and starts no backoff — dispatch refuses a
//!   cyclic wait before invoking `get_or_init`; if one somehow reaches the
//!   slot, it is returned but not stored.

use std::{future::Future, sync::Arc, time::Duration};

use thiserror::Error;
use tokio::sync::Mutex;
use wafer_block::core_types::{ErrorCode, WaferError};

use crate::platform::Instant;

/// Delay before the first retry after a transient init failure.
pub const TRANSIENT_RETRY_BASE: Duration = Duration::from_millis(100);

/// Longest delay between retries after consecutive transient init failures.
pub const TRANSIENT_RETRY_MAX: Duration = Duration::from_secs(30);

/// Error returned by a block's init closure, or by the slot itself.
#[derive(Debug, Clone, Error)]
pub enum InitError {
    /// A permanent failure: a missing required config key, a
    /// `lifecycle(Init)` error whose code says retrying cannot help, or a
    /// panic in `lifecycle(Init)` (caught on native targets).
    /// Cached for the slot's lifetime — the block will not be retried.
    #[error("permanent init failure: {0}")]
    Permanent(String),

    /// A transient failure: the config source could not be reached, or
    /// `lifecycle(Init)` failed with a code that says a retry may succeed
    /// (see [`InitError::from_lifecycle_error`]).
    /// NOT cached — a caller after the retry backoff runs init again.
    #[error("transient init failure: {0}")]
    Transient(String),

    /// An init cycle was detected (block A's init calls block B whose init
    /// calls block A, in one dispatch or across concurrent ones). Not
    /// cached — refused at dispatch level before the slot is locked.
    #[error("init cycle detected: {path:?}")]
    Cycle {
        /// Sequence of block names forming the detected init cycle.
        path: Vec<String>,
    },
}

impl InitError {
    /// Classify a `lifecycle(Init)` failure by its code.
    ///
    /// `Unavailable`, `DeadlineExceeded`, `Cancelled`, `ResourceExhausted` and
    /// `Aborted` say the attempt met a condition that can clear on its own (a
    /// backend down or busy, a budget spent, a conflict), so they are
    /// [`Transient`](Self::Transient). Every other code describes the block
    /// or its configuration, which a retry does not change, so it is
    /// [`Permanent`](Self::Permanent).
    pub fn from_lifecycle_error(e: &WaferError) -> Self {
        let msg = format!("lifecycle init failed: {e}");
        match e.code {
            ErrorCode::Unavailable
            | ErrorCode::DeadlineExceeded
            | ErrorCode::Cancelled
            | ErrorCode::ResourceExhausted
            | ErrorCode::Aborted => Self::Transient(msg),
            _ => Self::Permanent(msg),
        }
    }
}

/// The opaque result of a successful `lifecycle(Init)`.
///
/// Today this carries no data — the runtime only needs to know "init has
/// completed successfully" before dispatching normal messages. Future tasks
/// may add initialized handles or capabilities here.
#[derive(Debug, Clone, Default)]
pub struct InitializedState;

impl InitializedState {
    /// Construct the (currently unit-shaped) successful init state.
    pub fn new() -> Self {
        Self
    }
}

/// Outcomes that are cached for the slot's lifetime.
#[derive(Debug, Clone)]
enum CachedOutcome {
    Ok(InitializedState),
    Permanent(String),
}

/// The retry backoff a transient init failure starts.
#[derive(Debug, Clone)]
struct TransientBackoff {
    /// Consecutive transient failures so far (at least 1).
    failures: u32,
    /// No init attempt runs before this instant.
    retry_at: Instant,
    /// The last transient failure, returned to callers inside the window.
    message: String,
}

impl TransientBackoff {
    /// The backoff after `failures` consecutive transient failures:
    /// [`TRANSIENT_RETRY_BASE`] doubled per failure after the first, capped
    /// at [`TRANSIENT_RETRY_MAX`].
    fn delay(failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(16);
        TRANSIENT_RETRY_BASE
            .saturating_mul(1u32 << doublings)
            .min(TRANSIENT_RETRY_MAX)
    }

    /// The failure a caller inside the window gets, or `None` once the
    /// window has elapsed at `now`.
    fn pending(&self, now: Instant) -> Option<InitError> {
        (now < self.retry_at).then(|| {
            let wait = self.retry_at.saturating_duration_since(now);
            InitError::Transient(format!(
                "{} (init is retried in {} ms)",
                self.message,
                wait.as_millis()
            ))
        })
    }
}

/// Everything the slot's mutex guards.
#[derive(Debug, Default)]
struct SlotState {
    /// The cached `Ok` / `Permanent` outcome, once there is one.
    outcome: Option<CachedOutcome>,
    /// The backoff of the last run of consecutive transient failures.
    backoff: Option<TransientBackoff>,
}

impl SlotState {
    /// The answer a caller gets without running init: the cached outcome,
    /// or the pending transient failure while its backoff lasts.
    fn settled(&self, now: Instant) -> Option<Result<InitializedState, InitError>> {
        if let Some(cached) = &self.outcome {
            return Some(match cached {
                CachedOutcome::Ok(state) => Ok(state.clone()),
                CachedOutcome::Permanent(msg) => Err(InitError::Permanent(msg.clone())),
            });
        }
        self.backoff.as_ref().and_then(|b| b.pending(now)).map(Err)
    }
}

/// Per-block init slot.
///
/// Wraps a `tokio::sync::Mutex` over the slot's state. The mutex serializes
/// init attempts across concurrent first-callers: the second caller waits for
/// the first to finish rather than racing. Once an `Ok` or `Permanent`
/// outcome is stored — or while a transient failure's backoff lasts —
/// callers return immediately without re-running init.
#[derive(Debug, Default)]
pub struct BlockSlot {
    state: Arc<Mutex<SlotState>>,
}

impl BlockSlot {
    /// Build an empty slot with no cached outcome.
    pub fn new() -> Self {
        Self::default()
    }

    /// Lock-free-ish fast path: return the settled outcome — the cached one,
    /// or the pending transient failure inside its backoff — if the mutex is
    /// uncontended; `None` means "unknown — take the
    /// [`get_or_init`](Self::get_or_init) slow path".
    ///
    /// A held mutex means an init attempt is in flight, and an unsettled
    /// slot means init may run now — both correctly route the caller to
    /// `get_or_init`, which re-checks under the lock. This lets dispatch
    /// paths skip building the init context entirely once a block's init
    /// outcome is settled (PERF-03).
    pub fn try_cached(&self) -> Option<Result<InitializedState, InitError>> {
        let guard = self.state.try_lock().ok()?;
        guard.settled(Instant::now())
    }

    /// Run `init` unless the outcome is settled, otherwise return it.
    ///
    /// - `Ok(_)` → cached; all future callers see the same `InitializedState`.
    /// - `Err(Permanent(_))` → cached; all future callers see the same error.
    /// - `Err(Transient(_))` → not cached; callers inside the retry backoff
    ///   get it back without init running, the first caller after it runs
    ///   init again.
    /// - `Err(Cycle { .. })` → not cached, no backoff; returned as-is.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "async Mutex intentionally held across init().await to serialize concurrent first-time init; see module doc"
    )]
    pub async fn get_or_init<F, Fut>(&self, init: F) -> Result<InitializedState, InitError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<InitializedState, InitError>>,
    {
        let mut guard = self.state.lock().await;

        // Fast path: outcome cached, or a transient failure still backing off.
        if let Some(settled) = guard.settled(Instant::now()) {
            return settled;
        }

        // Slow path: run init while holding the mutex so concurrent callers
        // serialize (see module-level doc for rationale).
        match init().await {
            Ok(state) => {
                guard.outcome = Some(CachedOutcome::Ok(state.clone()));
                guard.backoff = None;
                Ok(state)
            }
            Err(InitError::Permanent(msg)) => {
                guard.outcome = Some(CachedOutcome::Permanent(msg.clone()));
                guard.backoff = None;
                Err(InitError::Permanent(msg))
            }
            Err(InitError::Transient(msg)) => {
                let failures = guard.backoff.as_ref().map_or(0, |b| b.failures) + 1;
                guard.backoff = Some(TransientBackoff {
                    failures,
                    retry_at: Instant::now() + TransientBackoff::delay(failures),
                    message: msg.clone(),
                });
                Err(InitError::Transient(msg))
            }
            Err(other @ InitError::Cycle { .. }) => {
                // Dispatch refuses a cyclic wait before reaching the slot.
                // If one does reach here, do not cache it.
                Err(other)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_the_base_and_caps() {
        assert_eq!(TransientBackoff::delay(1), TRANSIENT_RETRY_BASE);
        assert_eq!(TransientBackoff::delay(2), TRANSIENT_RETRY_BASE * 2);
        assert_eq!(TransientBackoff::delay(3), TRANSIENT_RETRY_BASE * 4);
        assert_eq!(TransientBackoff::delay(40), TRANSIENT_RETRY_MAX);
        assert_eq!(TransientBackoff::delay(u32::MAX), TRANSIENT_RETRY_MAX);
    }

    #[test]
    fn lifecycle_codes_that_can_clear_are_transient() {
        for code in [
            ErrorCode::Unavailable,
            ErrorCode::DeadlineExceeded,
            ErrorCode::Cancelled,
            ErrorCode::ResourceExhausted,
            ErrorCode::Aborted,
        ] {
            let e = InitError::from_lifecycle_error(&WaferError::new(code, "x"));
            assert!(matches!(e, InitError::Transient(_)), "{code:?} → {e:?}");
        }
        for code in [
            ErrorCode::Internal,
            ErrorCode::InvalidArgument,
            ErrorCode::FailedPrecondition,
            ErrorCode::PermissionDenied,
            ErrorCode::NotFound,
            ErrorCode::Unknown,
        ] {
            let e = InitError::from_lifecycle_error(&WaferError::new(code, "x"));
            assert!(matches!(e, InitError::Permanent(_)), "{code:?} → {e:?}");
        }
    }
}
