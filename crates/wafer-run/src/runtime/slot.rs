//! Per-block init state container with lazy-once-success semantics.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §3, §4
//!
//! ## Concurrency model — pattern (a): `Mutex<Option<CachedOutcome>>`
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
//! ## Semantics
//! - First `Ok(_)` result is cached for the slot's lifetime.
//! - `Permanent` error is cached for the slot's lifetime.
//! - `Transient` error is NOT cached — next call retries.
//! - `Cycle` error is NOT cached — dispatch checks the init-stack before
//!   invoking `get_or_init`; if one somehow reaches the slot, it is returned
//!   but not stored.

use std::{future::Future, sync::Arc};

use thiserror::Error;
use tokio::sync::Mutex;

/// Error returned by a block's init closure, or by the slot itself.
#[derive(Debug, Clone, Error)]
pub enum InitError {
    /// A permanent failure (e.g., missing required config, block panic).
    /// Cached for the slot's lifetime — the block will not be retried.
    #[error("permanent init failure: {0}")]
    Permanent(String),

    /// A transient failure (e.g., D1 timeout, network error).
    /// NOT cached — the next caller will retry init.
    #[error("transient init failure: {0}")]
    Transient(String),

    /// An init cycle was detected (block A's init calls block B whose init
    /// calls block A). Not cached — handled at dispatch level via `InitStack`.
    #[error("init cycle detected: {path:?}")]
    Cycle {
        /// Sequence of block names forming the detected init cycle.
        path: Vec<String>,
    },
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

/// Per-block init slot.
///
/// Wraps a `tokio::sync::Mutex<Option<CachedOutcome>>`. The mutex serializes
/// init attempts across concurrent first-callers: the second caller waits for
/// the first to finish rather than racing. Once an `Ok` or `Permanent` outcome
/// is stored, subsequent callers return immediately from the fast path without
/// re-running init.
#[derive(Debug, Default)]
pub struct BlockSlot {
    state: Arc<Mutex<Option<CachedOutcome>>>,
}

impl BlockSlot {
    /// Build an empty slot with no cached outcome.
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `init` if no cached outcome exists, otherwise return the cached result.
    ///
    /// - `Ok(_)` → cached; all future callers see the same `InitializedState`.
    /// - `Err(Permanent(_))` → cached; all future callers see the same error.
    /// - `Err(Transient(_))` → not cached; the next caller retries init.
    /// - `Err(Cycle { .. })` → not cached; returned as-is.
    pub async fn get_or_init<F, Fut>(&self, init: F) -> Result<InitializedState, InitError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<InitializedState, InitError>>,
    {
        let mut guard = self.state.lock().await;

        // Fast path: outcome already cached.
        if let Some(cached) = guard.as_ref() {
            return match cached {
                CachedOutcome::Ok(state) => Ok(state.clone()),
                CachedOutcome::Permanent(msg) => Err(InitError::Permanent(msg.clone())),
            };
        }

        // Slow path: run init while holding the mutex so concurrent callers
        // serialize (see module-level doc for rationale).
        match init().await {
            Ok(state) => {
                *guard = Some(CachedOutcome::Ok(state.clone()));
                Ok(state)
            }
            Err(InitError::Permanent(msg)) => {
                *guard = Some(CachedOutcome::Permanent(msg.clone()));
                Err(InitError::Permanent(msg))
            }
            Err(other @ InitError::Transient(_)) => {
                // Do NOT cache. Next caller will retry.
                Err(other)
            }
            Err(other @ InitError::Cycle { .. }) => {
                // Cycle errors should never reach the slot — dispatch checks
                // the init-stack before invoking get_or_init. If one does
                // reach here, do not cache it.
                Err(other)
            }
        }
    }
}
