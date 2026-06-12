//! WaferFlow — parser, validator, and expression evaluator for declarative
//! flow definitions.
//!
//! A WaferFlow is a JSON document describing a directed sequence of block
//! invocations: each [`Step`] names a block, supplies an `input` template
//! (possibly containing `$.step-id.field` references), and optionally routes
//! to other steps via conditional `next` entries. The runtime feeds step
//! outputs into an [`Accumulator`], which resolves references and evaluates
//! `when` conditions for the next step.
//!
//! Entry points:
//!
//! - [`parse`] — deserialize a JSON document into a [`WaferFlow`].
//! - [`validate`] — check structural invariants (unique step ids, known
//!   `next` targets, default branch present, well-formed expressions).
//! - [`Accumulator`] — runtime state for resolving `$.` references and
//!   evaluating `when`/`each` expressions.

#![warn(missing_docs)]

pub mod accumulator;
pub mod error;
pub(crate) mod expr;
pub mod parser;
pub mod types;
pub mod validate;

pub use accumulator::Accumulator;
pub use error::{ExprError, ParseError, ValidationError};
pub use parser::parse;
pub use types::{
    ConfigMapEntry, FlowConfig, FlowInfo, NextEntry, PortSchema, Step, WaferFlow,
};
pub use validate::validate;
