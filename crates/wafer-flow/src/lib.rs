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
//! - [`compiled`] — compile-once forms of `when`/`each`/`input` expressions
//!   for executors that parse at seal time and evaluate per step.
//! - `json_schema` (feature `json-schema`) — the JSON Schema a flow
//!   document must satisfy to parse, derived from the types in [`types`].

#![warn(missing_docs)]

pub mod accumulator;
pub mod compiled;
pub mod error;
pub(crate) mod expr;
pub mod parser;
pub mod types;
pub mod validate;

pub use accumulator::Accumulator;
pub use compiled::{CompiledCondition, CompiledPath, CompiledTemplate};
pub use error::{ExprError, InvalidTimeout, ParseError, ValidationError};
pub use parser::parse;
pub use types::{
    ConfigMapEntry, FlowConfig, FlowInfo, FlowTimeout, FlowTimeoutMillis, NextEntry, OnError,
    PortSchema, Step, WaferFlow, MAX_FLOW_TIMEOUT,
};
pub use validate::validate;

/// The JSON Schema (draft 2020-12) a WaferFlow document must satisfy to
/// [`parse()`], derived from [`WaferFlow`] under the deserialize contract.
///
/// The schema covers what parsing checks, plus two [`validate()`] rules it can
/// state: `steps` is non-empty and each `next` entry names exactly one of
/// `step` / `flow`. The other semantic rules (unique and unreserved step
/// ids, known jump targets, well-formed expressions, a default `next`
/// entry, a 24h ceiling on `timeout`, not both `timeout` and `timeout_ms`)
/// stay with [`validate()`] and [`parse()`].
#[cfg(feature = "json-schema")]
pub fn json_schema() -> serde_json::Value {
    schemars::generate::SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator()
        .into_root_schema_for::<WaferFlow>()
        .to_value()
}

#[cfg(all(test, feature = "json-schema"))]
mod json_schema_tests {
    use serde_json::{json, Value};

    use super::{json_schema, parse};

    fn flow_with_config(config: &Value) -> String {
        json!({
            "id": "f", "name": "F", "version": "0.1.0",
            "steps": [{ "id": "a", "block": "b" }],
            "config": config,
        })
        .to_string()
    }

    fn flow_config_schema() -> Value {
        json_schema()["$defs"]["FlowConfig"].clone()
    }

    /// Every `on_error` value the schema lists parses, and the list is the
    /// parser's: a value it omits does not parse.
    #[test]
    fn on_error_values_are_the_parsers() {
        let schema = json_schema();
        let listed: Vec<&str> = schema["$defs"]["OnError"]["oneOf"]
            .as_array()
            .expect("OnError is a oneOf of consts")
            .iter()
            .map(|variant| variant["const"].as_str().expect("a string const"))
            .collect();
        assert_eq!(listed, ["stop", "continue"]);
        for value in &listed {
            parse(&flow_with_config(&json!({ "on_error": value })))
                .unwrap_or_else(|e| panic!("listed on_error {value:?} must parse: {e}"));
        }
        for value in ["skip", "retry", "Stop"] {
            assert!(
                parse(&flow_with_config(&json!({ "on_error": value }))).is_err(),
                "unlisted on_error {value:?} must not parse"
            );
        }
    }

    /// The schema's `timeout_ms` bounds are the parser's: both bounds parse,
    /// one past either does not.
    #[test]
    fn timeout_ms_bounds_are_the_parsers() {
        let timeout_ms = &flow_config_schema()["properties"]["timeout_ms"];
        let min = timeout_ms["minimum"].as_u64().expect("a minimum");
        let max = timeout_ms["maximum"].as_u64().expect("a maximum");
        for ok in [min, max] {
            parse(&flow_with_config(&json!({ "timeout_ms": ok })))
                .unwrap_or_else(|e| panic!("timeout_ms {ok} must parse: {e}"));
        }
        for bad in [min - 1, max + 1] {
            assert!(
                parse(&flow_with_config(&json!({ "timeout_ms": bad }))).is_err(),
                "timeout_ms {bad} must not parse"
            );
        }
    }

    /// `FlowConfig` refuses unknown keys, and the schema lists every key
    /// the parser accepts.
    #[test]
    fn flow_config_keys_are_the_parsers() {
        let schema = flow_config_schema();
        assert_eq!(schema["additionalProperties"], json!(false));
        let samples = json!({
            "timeout": "30s",
            "timeout_ms": 1000,
            "max_steps": 5,
            "on_error": "continue",
        });
        let properties = schema["properties"].as_object().expect("properties");
        let mut listed: Vec<&String> = properties.keys().collect();
        listed.sort();
        let mut sampled: Vec<&String> = samples.as_object().unwrap().keys().collect();
        sampled.sort();
        assert_eq!(listed, sampled, "a FlowConfig key without a sample here");
        for (key, value) in samples.as_object().unwrap() {
            parse(&flow_with_config(&json!({ key: value })))
                .unwrap_or_else(|e| panic!("config key {key:?} must parse: {e}"));
        }
        assert!(parse(&flow_with_config(&json!({ "retries": 3 }))).is_err());
    }
}
