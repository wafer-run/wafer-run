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
    /// The detail code ([`crate::WaferError::detail_code`]) of the error the
    /// attempt failed with: a typed account of the failure that, unlike
    /// `message`, is safe to act on.
    ///
    /// - [`DATABASE_UNAVAILABLE`](crate::wire::database::DATABASE_UNAVAILABLE):
    ///   the database itself could not answer — a block's `lifecycle(Init)`
    ///   that propagated its database's outage, the config source's read
    ///   that met one, or a block's `Init` that propagated another block's
    ///   refusal carrying it.
    /// - [`CALL_BUDGET_EXHAUSTED`](crate::wire::database::CALL_BUDGET_EXHAUSTED):
    ///   the invocation's database call budget was spent.
    /// - `None`: the failure has no detail code — an `Unavailable` a block
    ///   raised for a dependency of its own, a database error a block wrapped
    ///   in a new `WaferError` (which drops it), a config source failure
    ///   with no coded cause, or an init budget overrun with no failure of
    ///   its own.
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

    /// This failure with `detail_code`.
    #[must_use]
    pub fn with_detail_code(mut self, detail_code: Option<impl Into<String>>) -> Self {
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
