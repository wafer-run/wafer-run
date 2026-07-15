//! Compile-once forms of flow expressions (PERF-03).
//!
//! The executor historically re-parsed every `when` condition, `each` path,
//! and `$.`-prefixed `input` template string on every step visit. These types
//! parse once — at seal time — and evaluate many times against an
//! [`Accumulator`].
//!
//! **Behavioral contract**: compiling is transparent. A string that fails to
//! parse is *not* rejected at compile time; the failure is stored and
//! reproduced on every evaluation, exactly as the parse-at-use APIs
//! ([`Accumulator::resolve`], [`Accumulator::resolve_input`],
//! [`Accumulator::eval_condition`]) behave. Those APIs delegate to these
//! types so the semantics have a single source.

use serde_json::Value;

use crate::{error::ExprError, expr, Accumulator};

/// A `$.step-id.field` path expression parsed once.
///
/// Compiled counterpart of [`Accumulator::resolve`].
#[derive(Debug, Clone)]
pub struct CompiledPath {
    inner: Result<Vec<String>, ExprError>,
}

impl CompiledPath {
    /// Parse `path` once. Invalid input is preserved — [`resolve`]
    /// reproduces the original parse error on every call.
    ///
    /// [`resolve`]: Self::resolve
    pub fn compile(path: &str) -> Self {
        Self {
            inner: expr::parse_path(path),
        }
    }

    /// Resolve against `acc`, exactly as `acc.resolve(path)` would.
    pub fn resolve(&self, acc: &Accumulator) -> Result<Value, ExprError> {
        match &self.inner {
            Ok(segments) => acc.resolve_segments(segments),
            Err(e) => Err(e.clone()),
        }
    }
}

/// A `when` condition expression parsed once.
///
/// Compiled counterpart of [`Accumulator::eval_condition`].
#[derive(Debug, Clone)]
pub struct CompiledCondition {
    inner: Result<expr::Expr, ExprError>,
}

impl CompiledCondition {
    /// Parse `condition` once. Invalid input is preserved — [`eval`]
    /// reproduces the original parse error on every call.
    ///
    /// [`eval`]: Self::eval
    pub fn compile(condition: &str) -> Self {
        Self {
            inner: expr::parse_expr(condition),
        }
    }

    /// Evaluate against `acc`, exactly as `acc.eval_condition(condition)`
    /// would: `Bool` is returned as-is, `Null` is `false`, any other value
    /// is `true`.
    pub fn eval(&self, acc: &Accumulator) -> Result<bool, ExprError> {
        match &self.inner {
            Ok(parsed) => {
                let result = expr::eval(parsed, &|segments| acc.resolve_segments(segments))?;
                Ok(match result {
                    Value::Bool(b) => b,
                    Value::Null => false,
                    _ => true,
                })
            }
            Err(e) => Err(e.clone()),
        }
    }
}

/// A step `input` template compiled once: `$.`-prefixed string leaves are
/// parsed to path segments up front, and subtrees containing no references
/// are collapsed to literals cloned wholesale at resolution.
///
/// Compiled counterpart of [`Accumulator::resolve_input`].
#[derive(Debug, Clone)]
pub struct CompiledTemplate {
    inner: TemplateNode,
}

#[derive(Debug, Clone)]
enum TemplateNode {
    /// A `$.path` string leaf, parsed.
    Path(Vec<String>),
    /// A `$.path` string leaf that failed to parse — resolution reproduces
    /// the parse error lazily, matching parse-at-use behavior.
    InvalidPath(ExprError),
    /// A subtree containing no `$.` references: cloned as-is.
    Literal(Value),
    /// An object with at least one `$.` reference below it. Entries are in
    /// the source map's iteration order so resolution errors surface in the
    /// same order as the uncompiled walk.
    Object(Vec<(String, TemplateNode)>),
    /// An array with at least one `$.` reference below it.
    Array(Vec<TemplateNode>),
}

impl CompiledTemplate {
    /// Walk `template` once, parsing every `$.` string leaf.
    pub fn compile(template: &Value) -> Self {
        Self {
            inner: compile_node(template),
        }
    }

    /// Resolve against `acc`, exactly as `acc.resolve_input(template)` would.
    pub fn resolve(&self, acc: &Accumulator) -> Result<Value, ExprError> {
        resolve_node(&self.inner, acc)
    }
}

/// True if the value or any descendant is a `$.`-prefixed string.
fn has_refs(v: &Value) -> bool {
    match v {
        Value::String(s) => s.starts_with("$."),
        Value::Object(map) => map.values().any(has_refs),
        Value::Array(arr) => arr.iter().any(has_refs),
        _ => false,
    }
}

fn compile_node(v: &Value) -> TemplateNode {
    match v {
        Value::String(s) if s.starts_with("$.") => match expr::parse_path(s) {
            Ok(segments) => TemplateNode::Path(segments),
            Err(e) => TemplateNode::InvalidPath(e),
        },
        Value::Object(map) if has_refs(v) => TemplateNode::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), compile_node(v)))
                .collect(),
        ),
        Value::Array(arr) if has_refs(v) => {
            TemplateNode::Array(arr.iter().map(compile_node).collect())
        }
        other => TemplateNode::Literal(other.clone()),
    }
}

fn resolve_node(node: &TemplateNode, acc: &Accumulator) -> Result<Value, ExprError> {
    match node {
        TemplateNode::Path(segments) => acc.resolve_segments(segments),
        TemplateNode::InvalidPath(e) => Err(e.clone()),
        TemplateNode::Literal(v) => Ok(v.clone()),
        TemplateNode::Object(entries) => {
            let mut map = serde_json::Map::new();
            for (k, n) in entries {
                map.insert(k.clone(), resolve_node(n, acc)?);
            }
            Ok(Value::Object(map))
        }
        TemplateNode::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for n in items {
                out.push(resolve_node(n, acc)?);
            }
            Ok(Value::Array(out))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn acc() -> Accumulator {
        let mut acc = Accumulator::new();
        acc.set("input", json!({ "name": "Alice", "n": 10 }));
        acc.set("step1", json!({ "token": "abc123" }));
        acc
    }

    #[test]
    fn compiled_template_matches_resolve_input() {
        let template = json!({
            "user": "$.input.name",
            "token": "$.step1.token",
            "static_value": 42,
            "nested": { "ref": "$.input.name", "plain": [1, 2, 3] },
            "literal_subtree": { "a": { "b": "c" } }
        });
        let acc = acc();
        let compiled = CompiledTemplate::compile(&template).resolve(&acc).unwrap();
        let direct = acc.resolve_input(&template).unwrap();
        assert_eq!(compiled, direct);
        assert_eq!(compiled["user"], json!("Alice"));
        assert_eq!(compiled["literal_subtree"], json!({ "a": { "b": "c" } }));
    }

    #[test]
    fn compiled_template_preserves_unresolved_reference_errors() {
        let template = json!({ "missing": "$.nope.field" });
        let acc = acc();
        let err = CompiledTemplate::compile(&template)
            .resolve(&acc)
            .unwrap_err();
        let direct_err = acc.resolve_input(&template).unwrap_err();
        assert_eq!(err.to_string(), direct_err.to_string());
    }

    #[test]
    fn compiled_template_preserves_parse_errors_lazily() {
        // "$." with no segments fails to parse; the error must surface at
        // resolution (parse-at-use parity), identically on every call.
        let template = json!({ "bad": "$." });
        let acc = acc();
        let compiled = CompiledTemplate::compile(&template);
        let e1 = compiled.resolve(&acc).unwrap_err().to_string();
        let e2 = compiled.resolve(&acc).unwrap_err().to_string();
        let direct = acc.resolve_input(&template).unwrap_err().to_string();
        assert_eq!(e1, direct);
        assert_eq!(e2, direct);
    }

    #[test]
    fn compiled_condition_matches_eval_condition() {
        let acc = acc();
        for cond in [
            "$.input.n > 5",
            "$.input.n < 5",
            "$.input.name == \"Alice\"",
            "$.step1.token",
        ] {
            assert_eq!(
                CompiledCondition::compile(cond).eval(&acc).unwrap(),
                acc.eval_condition(cond).unwrap(),
                "condition parity for {cond}"
            );
        }
    }

    #[test]
    fn compiled_condition_preserves_parse_errors() {
        let acc = acc();
        let compiled = CompiledCondition::compile("");
        let err = compiled.eval(&acc).unwrap_err().to_string();
        let direct = acc.eval_condition("").unwrap_err().to_string();
        assert_eq!(err, direct);
    }

    #[test]
    fn compiled_path_matches_resolve() {
        let acc = acc();
        assert_eq!(
            CompiledPath::compile("$.input.name").resolve(&acc).unwrap(),
            acc.resolve("$.input.name").unwrap()
        );
        let err = CompiledPath::compile("not-a-path")
            .resolve(&acc)
            .unwrap_err()
            .to_string();
        let direct = acc.resolve("not-a-path").unwrap_err().to_string();
        assert_eq!(err, direct);
    }
}
