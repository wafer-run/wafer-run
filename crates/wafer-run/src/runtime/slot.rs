//! Per-block init state container with lazy-once-success semantics.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §3, §4
//!
//! ## Concurrency model
//!
//! A slot has two locks with separate jobs:
//!
//! - **The attempt lock** (`tokio::sync::Mutex<()>`) is held across the
//!   entire init call. Concurrent first-callers for the same block
//!   serialize: the second caller waits for the first to finish rather than
//!   both running init simultaneously. This is intentional: init is
//!   typically I/O (config fetch, DB migration), and running it twice
//!   concurrently would either fail or do redundant work — running two
//!   migrations concurrently on the same table is a bug, not an
//!   optimisation.
//! - **The state lock** (a synchronous `parking_lot::Mutex`) guards the
//!   settled outcome and the transient backoff. It is taken only for a read
//!   or a write of that state, never across an `.await`, so a reader —
//!   [`BlockSlot::try_cached`] on every dispatch,
//!   [`BlockSlot::init_state`] for a health report — never waits on an init
//!   attempt and never sees "in flight" merely because another reader is
//!   looking.
//!
//! Holding the attempt lock across init is what makes an init cycle
//! between two blocks a potential deadlock; the dispatch path refuses the
//! wait that would close one before it takes this lock (see the
//! crate-private `runtime::init_waits` module).
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

use std::{future::Future, time::Duration};

use thiserror::Error;
use wafer_block::{
    core_types::{ErrorCode, WaferError},
    wire::database::STATEMENT_BUDGET_EXHAUSTED,
    BlockInitState,
};

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
    /// Classify a `lifecycle(Init)` failure by
    /// [`init_failure_is_transient`] over its code and detail code.
    pub fn from_lifecycle_error(e: &WaferError) -> Self {
        let msg = format!("lifecycle init failed: {e}");
        if init_failure_is_transient(e.code, e.detail_code()) {
            Self::Transient(msg)
        } else {
            Self::Permanent(msg)
        }
    }
}

/// Whether a failure with error code `code` and detail code `detail` is
/// one a retry can clear — the one rule behind every transient/permanent
/// split of a block's init.
///
/// [`InitError::from_lifecycle_error`] applies it to a `lifecycle(Init)`
/// error. A [`ConfigSource`](crate::ConfigSource) applies it to its
/// backend's error to choose [`ConfigError::Transient`](crate::ConfigError::Transient)
/// (`true`) or [`ConfigError::SourceFault`](crate::ConfigError::SourceFault)
/// (`false`) — for a database-backed source, over `DatabaseError::code()`
/// and `DatabaseError::detail_code()` — so the config path and the
/// lifecycle path cannot classify the same refusal differently.
///
/// `Unavailable`, `DeadlineExceeded`, `Cancelled` and `Aborted` say the
/// attempt met a condition that can clear on its own (a backend down or
/// busy, a deadline or cancellation, a conflict): transient. Every other
/// code describes the block or its configuration, which a retry does not
/// change: permanent.
///
/// # The `ResourceExhausted` rule
///
/// The detail code [`STATEMENT_BUDGET_EXHAUSTED`] is checked first, and is
/// **transient** whatever the code: the database refused a statement
/// because *this invocation* has spent its per-invocation statement budget,
/// and a fresh invocation (a rebuild in a new request) may run it.
///
/// Every other `ResourceExhausted` stays **permanent**: the runtime's own
/// limits raise it (call depth, the wasm host-memory budget, a flow's
/// `max_steps`), and an Init that exceeds one does so again on every run.
/// So does a statement no invocation can run at all — the database's
/// `StatementLimitExceeded`, answered as `InvalidArgument` with
/// `STATEMENT_BUDGET_EXCEEDS_LIMIT`. An Init that meets another limit which
/// does clear (a rate limit) reports it as `Unavailable`.
#[must_use]
pub fn init_failure_is_transient(code: ErrorCode, detail: Option<&str>) -> bool {
    if detail == Some(STATEMENT_BUDGET_EXHAUSTED) {
        return true;
    }
    matches!(
        code,
        ErrorCode::Unavailable
            | ErrorCode::DeadlineExceeded
            | ErrorCode::Cancelled
            | ErrorCode::Aborted
    )
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
                "{} (retry backoff: {} ms left)",
                self.message,
                wait.as_millis()
            ))
        })
    }
}

/// Everything the slot's state lock guards.
#[derive(Debug, Default)]
struct SlotState {
    /// The cached `Ok` / `Permanent` outcome, once there is one.
    outcome: Option<CachedOutcome>,
    /// The backoff of the last run of consecutive transient failures.
    backoff: Option<TransientBackoff>,
    /// An init attempt is running: set by [`BlockSlot::get_or_init`] once it
    /// holds the attempt lock and finds the slot unsettled, and cleared in
    /// the same state-lock critical section that records the attempt's
    /// outcome (or by [`InFlight`]'s drop when the attempt is cancelled).
    /// Readers learn "in flight" from it, never from the attempt lock.
    in_flight: bool,
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

/// Clears [`SlotState::in_flight`] when an init attempt is dropped before
/// it records an outcome (its dispatch was cancelled). A completed attempt
/// forgets it and clears the flag itself, together with the outcome.
struct InFlight<'a> {
    state: &'a parking_lot::Mutex<SlotState>,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.state.lock().in_flight = false;
    }
}

/// Per-block init slot.
///
/// The attempt lock (a `tokio::sync::Mutex`) serializes init attempts
/// across concurrent first-callers: the second caller waits for the first to
/// finish rather than racing. The outcome and backoff live under a separate
/// synchronous state lock, so once an `Ok` or `Permanent` outcome is stored
/// — or while a transient failure's backoff lasts — callers return
/// immediately without re-running init, and readers never wait on an
/// attempt (see the module doc).
#[derive(Debug, Default)]
pub struct BlockSlot {
    /// Held across one init attempt.
    attempt: tokio::sync::Mutex<()>,
    /// The settled outcome and the transient backoff. Never held across an
    /// `.await`.
    state: parking_lot::Mutex<SlotState>,
}

impl BlockSlot {
    /// Build an empty slot with no cached outcome.
    pub fn new() -> Self {
        Self::default()
    }

    /// Non-blocking fast path: return the settled outcome — the cached one,
    /// or the pending transient failure inside its backoff — or `None`
    /// meaning "not settled: take the [`get_or_init`](Self::get_or_init)
    /// slow path".
    ///
    /// `None` covers a slot init may run on now (never initialized, or past
    /// a transient failure's backoff) and one whose attempt is in flight;
    /// `get_or_init` waits for that attempt and re-checks. This lets
    /// dispatch paths skip building the init context entirely once a block's
    /// init outcome is settled (PERF-03). It never waits on an attempt.
    pub fn try_cached(&self) -> Option<Result<InitializedState, InitError>> {
        self.state.lock().settled(Instant::now())
    }

    /// How long until a transient failure's backoff lets init run again:
    /// `Some` while one is pending, `None` when none is (no transient
    /// failure, a cached outcome, or the backoff has elapsed).
    ///
    /// Under [`InitWait::Await`](crate::InitWait::Await) the dispatch paths
    /// name this delay as the `Retry-After` of the `Unavailable` a transient
    /// failure answers.
    pub fn pending_retry_in(&self) -> Option<Duration> {
        let state = self.state.lock();
        if state.outcome.is_some() {
            return None;
        }
        let now = Instant::now();
        state
            .backoff
            .as_ref()
            .filter(|b| now < b.retry_at)
            .map(|b| b.retry_at.saturating_duration_since(now))
    }

    /// Where this slot's init stands, read under the state lock alone —
    /// never the attempt lock, so it neither waits for nor holds up an init
    /// attempt, and one reader never makes another see "in flight":
    ///
    /// - a cached success → [`Ready`](BlockInitState::Ready);
    /// - a cached permanent failure → [`Failed`](BlockInitState::Failed);
    /// - a transient failure inside its backoff →
    ///   [`Retrying`](BlockInitState::Retrying) with the backoff left;
    /// - otherwise an attempt in flight →
    ///   [`Initializing`](BlockInitState::Initializing);
    /// - otherwise a transient failure whose backoff has elapsed →
    ///   `Retrying` with a zero `retry_in` (init may run again now);
    /// - otherwise [`NotInitialized`](BlockInitState::NotInitialized).
    pub fn init_state(&self) -> BlockInitState {
        let now = Instant::now();
        let last_transient = {
            let state = self.state.lock();
            match &state.outcome {
                Some(CachedOutcome::Ok(_)) => return BlockInitState::Ready,
                Some(CachedOutcome::Permanent(reason)) => {
                    return BlockInitState::Failed {
                        reason: reason.clone(),
                    };
                }
                None => {}
            }
            match &state.backoff {
                Some(b) if now < b.retry_at => {
                    return BlockInitState::Retrying {
                        reason: b.message.clone(),
                        retry_in: b.retry_at.saturating_duration_since(now),
                    };
                }
                _ if state.in_flight => return BlockInitState::Initializing,
                Some(b) => Some(b.message.clone()),
                None => None,
            }
        };
        match last_transient {
            Some(reason) => BlockInitState::Retrying {
                reason,
                retry_in: Duration::ZERO,
            },
            None => BlockInitState::NotInitialized,
        }
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
        reason = "async attempt lock intentionally held across init().await to serialize concurrent first-time init; see module doc"
    )]
    pub async fn get_or_init<F, Fut>(&self, init: F) -> Result<InitializedState, InitError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<InitializedState, InitError>>,
    {
        let _attempt = self.attempt.lock().await;

        // Fast path: outcome cached, or a transient failure still backing
        // off — including one an attempt this caller waited for just stored.
        // Otherwise mark the attempt in flight, in the same critical section
        // that found the slot unsettled.
        let in_flight = {
            let mut state = self.state.lock();
            if let Some(settled) = state.settled(Instant::now()) {
                return settled;
            }
            state.in_flight = true;
            InFlight { state: &self.state }
        };

        // Slow path: run init while holding the attempt lock so concurrent
        // callers serialize (see module-level doc for rationale).
        let outcome = init().await;
        // The flag is cleared below, under the lock that records the
        // outcome, so no reader sees the attempt over before its outcome
        // is stored; the guard only clears it for a cancelled attempt.
        std::mem::forget(in_flight);
        let mut state = self.state.lock();
        state.in_flight = false;
        match outcome {
            Ok(init_state) => {
                state.outcome = Some(CachedOutcome::Ok(init_state.clone()));
                state.backoff = None;
                Ok(init_state)
            }
            Err(InitError::Permanent(msg)) => {
                state.outcome = Some(CachedOutcome::Permanent(msg.clone()));
                state.backoff = None;
                Err(InitError::Permanent(msg))
            }
            Err(InitError::Transient(msg)) => {
                let failures = state.backoff.as_ref().map_or(0, |b| b.failures) + 1;
                state.backoff = Some(TransientBackoff {
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

/// The [`BlockSlot::init_state`] of every slot in `slots` (the runtime's
/// registered-name → slot map), keyed by registered name. Backs
/// `Wafer::init_states` and `Context::block_init_states`.
pub(crate) fn init_states(
    slots: &std::collections::HashMap<String, std::sync::Arc<BlockSlot>>,
) -> std::collections::BTreeMap<String, BlockInitState> {
    slots
        .iter()
        .map(|(name, slot)| (name.clone(), slot.init_state()))
        .collect()
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
            ErrorCode::Aborted,
        ] {
            let e = InitError::from_lifecycle_error(&WaferError::new(code, "x"));
            assert!(matches!(e, InitError::Transient(_)), "{code:?} → {e:?}");
        }
        for code in [
            ErrorCode::ResourceExhausted,
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

    /// The `ResourceExhausted` rule: a spent per-invocation statement
    /// budget is transient (checked before the code), a statement no
    /// invocation can run and the runtime's own limits stay permanent.
    #[test]
    fn a_spent_statement_budget_is_transient_other_limits_are_permanent() {
        use wafer_block::wire::database::STATEMENT_BUDGET_EXCEEDS_LIMIT;

        let spent = WaferError::new(ErrorCode::ResourceExhausted, "budget spent")
            .with_detail_code(STATEMENT_BUDGET_EXHAUSTED);
        assert!(matches!(
            InitError::from_lifecycle_error(&spent),
            InitError::Transient(_)
        ));

        let over_limit = WaferError::new(ErrorCode::InvalidArgument, "over the limit")
            .with_detail_code(STATEMENT_BUDGET_EXCEEDS_LIMIT);
        assert!(matches!(
            InitError::from_lifecycle_error(&over_limit),
            InitError::Permanent(_)
        ));

        let runtime_limit = WaferError::new(ErrorCode::ResourceExhausted, "call depth");
        assert!(matches!(
            InitError::from_lifecycle_error(&runtime_limit),
            InitError::Permanent(_)
        ));
    }

    /// The public rule a `ConfigSource` classifies its backend errors by is
    /// the one `from_lifecycle_error` applies.
    #[test]
    fn init_failure_is_transient_is_the_lifecycle_rule() {
        use wafer_block::wire::database::STATEMENT_BUDGET_EXCEEDS_LIMIT;

        let cases = [
            (ErrorCode::Unavailable, None, true),
            (
                ErrorCode::ResourceExhausted,
                Some(STATEMENT_BUDGET_EXHAUSTED),
                true,
            ),
            (ErrorCode::ResourceExhausted, None, false),
            (
                ErrorCode::InvalidArgument,
                Some(STATEMENT_BUDGET_EXCEEDS_LIMIT),
                false,
            ),
            (ErrorCode::Internal, None, false),
        ];
        for (code, detail, transient) in cases {
            assert_eq!(
                init_failure_is_transient(code, detail),
                transient,
                "{code:?} {detail:?}"
            );
            let mut e = WaferError::new(code, "x");
            if let Some(detail) = detail {
                e = e.with_detail_code(detail);
            }
            assert_eq!(
                matches!(InitError::from_lifecycle_error(&e), InitError::Transient(_)),
                transient,
                "{code:?} {detail:?}"
            );
        }
    }
}
