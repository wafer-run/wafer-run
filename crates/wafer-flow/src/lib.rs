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
/// A document the schema accepts parses, and one it rejects does not, with
/// one exception: JSON Schema counts `1000.0` as an integer, but the parser
/// refuses a fraction in an integer field (`timeout_ms`, `max_steps`).
///
/// The schema also states two [`validate()`] rules: `steps` is non-empty and
/// each `next` entry names exactly one of `step` / `flow`. The rest stay
/// with [`validate()`] alone: unique and unreserved step ids, known jump
/// targets, no `next` inside a parallel branch or back to the flow itself,
/// a default `next` entry, well-formed expressions, and not setting both
/// `timeout` and `timeout_ms`.
///
/// The schema has no `$id`: whoever publishes it adds the URL it is served
/// at.
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

    use super::{json_schema, parse, validate};

    /// Whether the runtime takes `document`: it parses and validates.
    fn runtime_accepts(document: &Value) -> bool {
        parse(&document.to_string()).is_ok_and(|flow| validate(&flow).is_ok())
    }

    fn schema_accepts(document: &Value) -> bool {
        jsonschema::draft202012::new(&json_schema())
            .expect("the generated schema compiles")
            .is_valid(document)
    }

    /// A valid one-step flow with `extra` merged into its top level.
    fn flow_with(extra: &Value) -> Value {
        let mut flow = json!({
            "id": "f", "name": "F", "version": "0.1.0",
            "steps": [{ "id": "a", "block": "b" }],
        });
        for (key, value) in extra.as_object().expect("extra is an object") {
            flow[key] = value.clone();
        }
        flow
    }

    fn flow_with_config(config: &Value) -> Value {
        flow_with(&json!({ "config": config }))
    }

    fn flow_with_next(next: &Value) -> Value {
        flow_with(&json!({
            "steps": [
                { "id": "a", "block": "b", "next": next },
                { "id": "c", "block": "d" },
            ],
        }))
    }

    /// Real flow documents from this repository: the schema must take every
    /// one the runtime takes.
    #[test]
    fn schema_accepts_the_repositorys_flows() {
        for (name, text) in [
            (
                "echo-flow",
                include_str!("../../wafer-ffi/testdata/echo-flow.json"),
            ),
            (
                "gated-flow",
                include_str!("../../wafer-ffi/testdata/gated-flow.json"),
            ),
            (
                "missing-block-flow",
                include_str!("../../wafer-ffi/testdata/missing-block-flow.json"),
            ),
        ] {
            let document: Value = serde_json::from_str(text).expect("testdata is JSON");
            assert!(
                runtime_accepts(&document),
                "{name}: the runtime must take it"
            );
            assert!(schema_accepts(&document), "{name}: the schema must take it");
        }
        let login = json!({
            "id": "login-flow", "name": "User Login", "version": "1.0.0",
            "description": "Authenticate a user",
            "input": {
                "type": "object",
                "properties": { "email": { "type": "string" }, "password": { "type": "string" } },
                "required": ["email", "password"],
            },
            "steps": [
                { "id": "find-user", "block": "database/query", "input": { "email": "$.input.email" } },
                {
                    "id": "check", "block": "crypto/verify",
                    "input": { "hash": "$.find-user.hash", "password": "$.input.password" },
                    "next": [
                        { "when": "$.check.match == true", "step": "token" },
                        { "step": "reject" },
                    ],
                },
                { "id": "token", "block": "crypto/jwt-sign", "each": "$.find-user.ids", "config": { "ttl": 60 } },
                {
                    "id": "fan", "block": "noop",
                    "parallel": [{ "steps": [{ "id": "left", "block": "l" }] }, { "steps": [{ "id": "right", "block": "r" }] }],
                },
                { "id": "reject", "block": "respond/error", "input": { "status": 401 } },
            ],
            "config": { "timeout": "30s", "max_steps": 100, "on_error": "continue" },
            "blocks": ["database/query"],
            "config_map": { "ttl": { "target": "crypto/jwt-sign", "key": "ttl_secs" } },
            "config_defaults": { "crypto/jwt-sign": { "alg": "HS256" } },
        });
        assert!(runtime_accepts(&login));
        assert!(schema_accepts(&login));
    }

    /// Edge cases on which the schema and the runtime must agree.
    #[test]
    fn schema_and_runtime_agree_on_edge_cases() {
        let cases: Vec<(&str, Value)> = vec![
            (
                "on_error stop",
                flow_with_config(&json!({ "on_error": "stop" })),
            ),
            (
                "on_error continue",
                flow_with_config(&json!({ "on_error": "continue" })),
            ),
            (
                "on_error skip",
                flow_with_config(&json!({ "on_error": "skip" })),
            ),
            (
                "on_error retry",
                flow_with_config(&json!({ "on_error": "retry" })),
            ),
            (
                "on_error Stop",
                flow_with_config(&json!({ "on_error": "Stop" })),
            ),
            (
                "on_error null",
                flow_with_config(&json!({ "on_error": null })),
            ),
            (
                "timeout 30s",
                flow_with_config(&json!({ "timeout": "30s" })),
            ),
            (
                "timeout 250ms",
                flow_with_config(&json!({ "timeout": "250ms" })),
            ),
            (
                "timeout bare 45",
                flow_with_config(&json!({ "timeout": "45" })),
            ),
            (
                "timeout 007m",
                flow_with_config(&json!({ "timeout": "007m" })),
            ),
            ("timeout 0s", flow_with_config(&json!({ "timeout": "0s" }))),
            (
                "timeout 000",
                flow_with_config(&json!({ "timeout": "000" })),
            ),
            (
                "timeout 0ms",
                flow_with_config(&json!({ "timeout": "0ms" })),
            ),
            (
                "timeout 24h",
                flow_with_config(&json!({ "timeout": "24h" })),
            ),
            (
                "timeout 25h",
                flow_with_config(&json!({ "timeout": "25h" })),
            ),
            (
                "timeout 1440m",
                flow_with_config(&json!({ "timeout": "1440m" })),
            ),
            (
                "timeout 1441m",
                flow_with_config(&json!({ "timeout": "1441m" })),
            ),
            (
                "timeout 86400s",
                flow_with_config(&json!({ "timeout": "86400s" })),
            ),
            (
                "timeout 86401s",
                flow_with_config(&json!({ "timeout": "86401s" })),
            ),
            (
                "timeout bare 86400",
                flow_with_config(&json!({ "timeout": "86400" })),
            ),
            (
                "timeout bare 86401",
                flow_with_config(&json!({ "timeout": "86401" })),
            ),
            (
                "timeout 86400000ms",
                flow_with_config(&json!({ "timeout": "86400000ms" })),
            ),
            (
                "timeout 86400001ms",
                flow_with_config(&json!({ "timeout": "86400001ms" })),
            ),
            (
                "timeout 1.5s",
                flow_with_config(&json!({ "timeout": "1.5s" })),
            ),
            (
                "timeout -1s",
                flow_with_config(&json!({ "timeout": "-1s" })),
            ),
            (
                "timeout 30 s",
                flow_with_config(&json!({ "timeout": "30 s" })),
            ),
            (
                "timeout 30S",
                flow_with_config(&json!({ "timeout": "30S" })),
            ),
            ("timeout 1d", flow_with_config(&json!({ "timeout": "1d" }))),
            ("timeout empty", flow_with_config(&json!({ "timeout": "" }))),
            (
                "timeout number",
                flow_with_config(&json!({ "timeout": 30 })),
            ),
            (
                "timeout_ms 1",
                flow_with_config(&json!({ "timeout_ms": 1 })),
            ),
            (
                "timeout_ms 0",
                flow_with_config(&json!({ "timeout_ms": 0 })),
            ),
            (
                "timeout_ms 86400000",
                flow_with_config(&json!({ "timeout_ms": 86_400_000 })),
            ),
            (
                "timeout_ms 86400001",
                flow_with_config(&json!({ "timeout_ms": 86_400_001 })),
            ),
            (
                "timeout_ms -5",
                flow_with_config(&json!({ "timeout_ms": -5 })),
            ),
            (
                "timeout_ms 1.5",
                flow_with_config(&json!({ "timeout_ms": 1.5 })),
            ),
            (
                "timeout_ms string",
                flow_with_config(&json!({ "timeout_ms": "1000" })),
            ),
            ("max_steps 1", flow_with_config(&json!({ "max_steps": 1 }))),
            ("max_steps 0", flow_with_config(&json!({ "max_steps": 0 }))),
            (
                "max_steps u64::MAX",
                flow_with_config(&json!({ "max_steps": u64::MAX })),
            ),
            ("max_steps above u64::MAX", {
                let text = flow_with_config(&json!({ "max_steps": 1 }))
                    .to_string()
                    .replace("\"max_steps\":1", "\"max_steps\":18446744073709551616");
                serde_json::from_str(&text).expect("JSON")
            }),
            (
                "config unknown key",
                flow_with_config(&json!({ "retries": 3 })),
            ),
            ("config null", flow_with(&json!({ "config": null }))),
            (
                "config not an object",
                flow_with(&json!({ "config": "stop" })),
            ),
            (
                "top-level unknown key",
                flow_with(&json!({ "owner": "me" })),
            ),
            (
                "step unknown key",
                flow_with(&json!({ "steps": [{ "id": "a", "block": "b", "retry": 2 }] })),
            ),
            ("no steps", flow_with(&json!({ "steps": [] }))),
            (
                "step without block",
                flow_with(&json!({ "steps": [{ "id": "a" }] })),
            ),
            (
                "missing name",
                json!({ "id": "f", "version": "1", "steps": [{ "id": "a", "block": "b" }] }),
            ),
            ("next to a step", flow_with_next(&json!([{ "step": "c" }]))),
            (
                "next to a flow",
                flow_with_next(&json!([{ "flow": "other" }])),
            ),
            (
                "next with both targets",
                flow_with_next(&json!([{ "step": "c", "flow": "other" }])),
            ),
            (
                "next with no target",
                flow_with_next(&json!([{ "when": "$.a.ok == true" }, { "step": "c" }])),
            ),
            (
                "next with a null step",
                flow_with_next(&json!([{ "step": null, "flow": "other" }])),
            ),
            (
                "next target a number",
                flow_with_next(&json!([{ "step": 3 }])),
            ),
            (
                "config_map entry",
                flow_with(&json!({ "config_map": { "k": { "target": "b", "key": "x" } } })),
            ),
            (
                "config_map entry without key",
                flow_with(&json!({ "config_map": { "k": { "target": "b" } } })),
            ),
            (
                "config_map entry a string",
                flow_with(&json!({ "config_map": { "k": "b.x" } })),
            ),
            (
                "config_defaults",
                flow_with(&json!({ "config_defaults": { "b": { "x": 1 } } })),
            ),
            ("blocks not strings", flow_with(&json!({ "blocks": [1] }))),
            (
                "port type not a string",
                flow_with(&json!({ "input": { "type": 1 } })),
            ),
            (
                "parallel branch without steps",
                flow_with(&json!({ "steps": [{ "id": "a", "block": "b", "parallel": [{}] }] })),
            ),
        ];
        for (name, document) in &cases {
            assert_eq!(
                schema_accepts(document),
                runtime_accepts(document),
                "{name}: the schema and the runtime disagree on {document}"
            );
        }
    }

    /// Where the schema knowingly differs from the runtime, as documented on
    /// [`json_schema`]. If one of these starts agreeing, the gap has closed:
    /// move it to the agreeing cases and fix the documentation.
    #[test]
    fn documented_gaps_are_the_only_disagreements() {
        let schema_only: Vec<(&str, Value)> = vec![
            (
                "integer with a fraction",
                flow_with_config(&json!({ "timeout_ms": 1000.0 })),
            ),
            (
                "max_steps with a fraction",
                flow_with_config(&json!({ "max_steps": 5.0 })),
            ),
            (
                "both timeouts",
                flow_with_config(&json!({ "timeout": "1s", "timeout_ms": 1000 })),
            ),
            (
                "duplicate step ids",
                flow_with(
                    &json!({ "steps": [{ "id": "a", "block": "b" }, { "id": "a", "block": "b" }] }),
                ),
            ),
            (
                "reserved step id",
                flow_with(&json!({ "steps": [{ "id": "input", "block": "b" }] })),
            ),
            (
                "unknown jump target",
                flow_with_next(&json!([{ "step": "nowhere" }])),
            ),
            (
                "no default next",
                flow_with_next(&json!([{ "when": "$.a.ok == true", "step": "c" }])),
            ),
            (
                "malformed expression",
                flow_with(&json!({ "steps": [{ "id": "a", "block": "b", "each": "not a path" }] })),
            ),
        ];
        for (name, document) in &schema_only {
            assert!(
                schema_accepts(document),
                "{name}: the schema should take it"
            );
            assert!(
                !runtime_accepts(document),
                "{name}: the runtime should refuse it"
            );
        }
    }
}
