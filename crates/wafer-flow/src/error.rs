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

    /// A `next` entry points at a `step` id that does not exist in the flow.
    #[error("step '{from}' references unknown step '{target}' in next")]
    UnknownNextTarget {
        /// The step that contains the offending `next` entry.
        from: String,
        /// The unknown step id referenced by the `next` entry.
        target: String,
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
