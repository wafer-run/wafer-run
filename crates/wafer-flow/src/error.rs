//! Error types returned by [`parse`](crate::parse), [`validate`](crate::validate),
//! and the expression evaluator.

use thiserror::Error;

/// Failure to deserialize a flow JSON document into a [`WaferFlow`](crate::WaferFlow).
#[derive(Debug, Error)]
pub enum ParseError {
    /// The input was not valid JSON, or did not match the [`WaferFlow`](crate::WaferFlow) shape.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// A `timeout` string in a flow's config that is not a positive duration
/// (see [`FlowTimeout`](crate::FlowTimeout)).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid flow timeout '{value}': {reason}")]
pub struct InvalidTimeout {
    /// The rejected text.
    pub value: String,
    /// Why it was rejected.
    pub reason: &'static str,
}

/// A semantic problem found by [`validate`](crate::validate).
///
/// [`validate`](crate::validate) returns *all* errors it finds, so callers
/// receive a `Vec<ValidationError>` rather than failing on the first issue.
#[derive(Debug, Error)]
pub enum ValidationError {
    /// Two steps share the same `id` (across the flow or inside `parallel` branches).
    #[error("duplicate step id: {0}")]
    DuplicateStepId(String),

    /// A step uses a reserved accumulator key as its `id`. `input` holds the
    /// caller's payload and `each` holds the per-item fan-out binding
    /// (`$.each.item` / `$.each.index`), so steps may not claim those names.
    #[error("step id '{0}' is reserved (accumulator key owned by the runtime)")]
    ReservedStepId(String),

    /// A `next` entry's `step` names no top-level step of the flow. Steps
    /// inside `parallel` branches are not jump targets.
    #[error("step '{from}' routes next to '{target}', which is not a top-level step of the flow")]
    UnknownNextTarget {
        /// The step that contains the offending `next` entry.
        from: String,
        /// The step id the `next` entry names.
        target: String,
    },

    /// A `next` entry names both a `step` and a `flow`.
    #[error("step '{0}' has a next entry with both 'step' and 'flow'")]
    NextEntryWithTwoTargets(String),

    /// A step inside a `parallel` branch has `next` entries. Branch steps
    /// run strictly in order, so `next` cannot route them.
    #[error("step '{0}' is inside a parallel branch, where 'next' routing is not allowed")]
    NextInParallelBranch(String),

    /// A `next` entry transfers control to the flow that contains it.
    #[error("step '{step}' transfers to its own flow '{flow}'; route to a step instead")]
    TransferToSelf {
        /// The step that contains the offending `next` entry.
        step: String,
        /// The flow's own id.
        flow: String,
    },

    /// A `next` entry has neither a `step` nor a `flow` target.
    #[error("step '{0}' has a next entry with neither 'step' nor 'flow'")]
    NextEntryMissingTarget(String),

    /// A step defines `next` entries but none of them is unconditional
    /// (an entry without a `when`).
    #[error("step '{0}' must have a default next entry (one without 'when') when next is present")]
    MissingDefaultNext(String),

    /// A `when`, `each`, or `input` expression failed to parse.
    #[error("invalid expression in step '{step}': {reason}")]
    InvalidExpression {
        /// The step whose expression is invalid.
        step: String,
        /// The parser error message.
        reason: String,
    },

    /// The flow config sets both `timeout` and `timeout_ms`.
    #[error("flow config sets both 'timeout' and 'timeout_ms'; set one")]
    ConflictingTimeouts,

    /// The flow contains zero steps.
    #[error("flow has no steps")]
    EmptyFlow,
}

/// A failure raised while parsing or evaluating a `$.`-prefixed reference
/// or `when` expression.
///
/// `Clone` so seal-time compiled forms ([`crate::compiled`]) can store a
/// parse failure once and reproduce it on every evaluation, matching
/// parse-at-use behavior.
#[derive(Debug, Clone, Error)]
pub enum ExprError {
    /// A `$.path` string was malformed (missing prefix, empty segment, etc.).
    #[error("invalid path expression: {0}")]
    InvalidPath(String),

    /// A `$.path` resolved against the [`Accumulator`](crate::Accumulator)
    /// pointed at a missing step or field.
    #[error("unresolved reference: {0}")]
    UnresolvedReference(String),

    /// An expression tried to combine incompatible value types (e.g. indexing
    /// a string, or comparing non-numeric values with `>`).
    #[error("type error: {0}")]
    TypeError(String),

    /// The expression parser could not tokenize the input.
    #[error("parse error: {0}")]
    Parse(String),
}
