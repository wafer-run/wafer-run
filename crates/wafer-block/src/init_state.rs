//! Per-block init state, as reported by the runtime.
//!
//! Returned by [`Context::block_init_states`](crate::context::Context::block_init_states)
//! (and the runtime's `Wafer::init_states`) so a health endpoint can say
//! which blocks are serving, which are waiting on a retry, and which failed,
//! without dispatching to any of them.

use std::time::Duration;

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
    /// unreachable, a backend down, a spent statement budget). The failure
    /// is not cached: init can run again.
    Retrying {
        /// The last transient failure's message. It may quote a backend's
        /// error; a public health body reports the class, not this text.
        reason: String,
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
