use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("duplicate step id: {0}")]
    DuplicateStepId(String),

    #[error("step '{from}' references unknown step '{target}' in next")]
    UnknownNextTarget { from: String, target: String },

    #[error("step '{0}' has a next entry with neither 'step' nor 'flow'")]
    NextEntryMissingTarget(String),

    #[error("step '{0}' must have a default next entry (one without 'when') when next is present")]
    MissingDefaultNext(String),

    #[error("invalid expression in step '{step}': {reason}")]
    InvalidExpression { step: String, reason: String },

    #[error("flow has no steps")]
    EmptyFlow,
}

#[derive(Debug, Error)]
pub enum ExprError {
    #[error("invalid path expression: {0}")]
    InvalidPath(String),

    #[error("unresolved reference: {0}")]
    UnresolvedReference(String),

    #[error("type error: {0}")]
    TypeError(String),

    #[error("parse error: {0}")]
    Parse(String),
}
