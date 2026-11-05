//! Per-block init state, as reported by the runtime.
//!
//! Returned by [`Context::block_init_states`](crate::context::Context::block_init_states)
//! (and the runtime's `Wafer::init_states`) so a health endpoint can say
//! which blocks are serving, which are waiting on a retry, and which failed,
//! without dispatching to any of them.

use std::time::Duration;

/// A transient `Init` failure: what failed, and its detail code. The one
/// shape the runtime keeps one in — the slot's `InitError::Transient`, its
/// retry backoff, and [`BlockInitState::Retrying`] — so the detail code is
/// carried wherever the failure is, never dropped on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransientFailure {
    /// What failed. It may quote a backend's error (a table's name, a host);
    /// a public body reports the class, not this text.
    pub message: String,
    /// The failure's class, as a detail code
    /// ([`crate::WaferError::detail_code`]): a typed account that, unlike
    /// `message`, is safe to act on. Exactly one of:
    ///
    /// - What the attempt's own database calls saw, when they saw anything
    ///   ([`crate::init_attempt`]). It takes precedence over whatever error
    ///   the block returned, so a block that wrapped its database's error, or
    ///   hung on a call until the init budget cut it off, still reads as the
    ///   database's failure. One of
    ///   [`DATABASE_UNAVAILABLE`](crate::wire::database::DATABASE_UNAVAILABLE)
    ///   (the database could not answer, or a call was still waiting on it
    ///   at the init budget),
    ///   [`DATABASE_CONTENDED`](crate::wire::database::DATABASE_CONTENDED)
    ///   (busy with other work), or
    ///   [`CALL_BUDGET_EXHAUSTED`](crate::wire::database::CALL_BUDGET_EXHAUSTED)
    ///   (the invocation's call budget spent).
    /// - Otherwise, the detail code of the error the attempt failed with,
    ///   when it had one: the config source's (a database-backed source
    ///   passes its read's, e.g. `database.unavailable`), a refused
    ///   dispatch's (`database.unavailable` only), or any code the block's
    ///   own error carried (`auth.invalid_token`, …).
    /// - `None`: none of those — every call the attempt made answered, and
    ///   the block failed on its own with no code, or the init budget ran out
    ///   with no call in flight and no failure of the block's own.
    ///
    /// The record's limits ([`crate::init_attempt`]'s "Limits") apply: it
    /// reports what the attempt's calls saw, which may be a failure the block
    /// handled rather than the one it failed on; it classes only a transient
    /// failure; and it does not see an attempt this one only waited on.
    pub detail_code: Option<String>,
}

impl TransientFailure {
    /// A failure with `message` and no detail code.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            detail_code: None,
        }
    }

    /// This failure with class `detail_code` (see [`Self::detail_code`]).
    #[must_use]
    pub fn classed(mut self, detail_code: Option<impl Into<String>>) -> Self {
        self.detail_code = detail_code.map(Into::into);
        self
    }
}

impl std::fmt::Display for TransientFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Where one block's `lifecycle(Init)` stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockInitState {
    /// Init has not run, and no attempt is in flight.
    NotInitialized,
    /// An init attempt is in flight.
    Initializing,
    /// Init succeeded; the block serves.
    Ready,
    /// The last init attempt failed transiently (the config source
    /// unreachable, a backend down, a spent per-invocation call budget). The
    /// failure is not cached: init can run again.
    Retrying {
        /// The last transient failure, with its detail code.
        failure: TransientFailure,
        /// What is left of the runtime's retry backoff for this block:
        /// zero once it has elapsed. Where dispatch initializes lazily
        /// (`InitWait::Await`) the first dispatch after it runs init again;
        /// where it never does (`InitWait::Refuse`) the backoff only spaces
        /// explicit `init_block` calls, and the embedder's rebuild of the
        /// runtime is the retry.
        retry_in: Duration,
    },
    /// Init failed permanently (a missing required key, a config source
    /// fault, a lifecycle error retrying cannot fix, a panic). Cached for
    /// the runtime's life: the block never serves in this runtime.
    Failed {
        /// The failure's message. It may quote a backend's error; a public
        /// health body reports the class, not this text.
        reason: String,
    },
}

impl BlockInitState {
    /// A stable lowercase name for the state (`"not_initialized"`,
    /// `"initializing"`, `"ready"`, `"retrying"`, `"failed"`), for reports
    /// and health bodies.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NotInitialized => "not_initialized",
            Self::Initializing => "initializing",
            Self::Ready => "ready",
            Self::Retrying { .. } => "retrying",
            Self::Failed { .. } => "failed",
        }
    }

    /// Whether the block serves dispatches.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}
