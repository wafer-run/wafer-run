use std::collections::HashMap;

use serde_json::Value;

use crate::error::ExprError;
use crate::expr;

/// Stores step outputs and resolves `$.` references against stored data.
#[derive(Debug, Clone)]
pub struct Accumulator {
    data: HashMap<String, Value>,
}

impl Accumulator {
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    /// Store a step's output under its id.
    pub fn set(&mut self, step_id: &str, value: Value) {
        self.data.insert(step_id.to_string(), value);
    }

    /// Get the raw value for a step id.
    pub fn get(&self, step_id: &str) -> Option<&Value> {
        self.data.get(step_id)
    }

    /// Resolve a path expression like `$.step-id.field.nested` to a value.
    pub fn resolve(&self, path: &str) -> Result<Value, ExprError> {
        let segments = expr::parse_path(path)?;
        self.resolve_segments(&segments)
    }

    /// Resolve path segments against stored data.
    pub fn resolve_segments(&self, segments: &[String]) -> Result<Value, ExprError> {
        if segments.is_empty() {
            return Err(ExprError::InvalidPath("empty path".into()));
        }

        let root_key = &segments[0];
        let root = self.data.get(root_key).ok_or_else(|| {
            ExprError::UnresolvedReference(format!("$.{root_key}"))
        })?;

        let mut current = root;
        for seg in &segments[1..] {
            current = match current {
                Value::Object(map) => map.get(seg.as_str()).ok_or_else(|| {
                    ExprError::UnresolvedReference(format!(
                        "$.{}",
                        segments[..segments.iter().position(|s| s == seg).unwrap() + 1].join(".")
                    ))
                })?,
                Value::Array(arr) => {
                    let idx: usize = seg.parse().map_err(|_| {
                        ExprError::TypeError(format!(
                            "cannot index array with non-numeric key '{seg}'"
                        ))
                    })?;
                    arr.get(idx).ok_or_else(|| {
                        ExprError::UnresolvedReference(format!("array index {idx} out of bounds"))
                    })?
                }
                _ => {
                    return Err(ExprError::TypeError(format!(
                        "cannot traverse into {current} with key '{seg}'"
                    )));
                }
            };
        }

        Ok(current.clone())
    }

    /// Walk a JSON value, resolving all `$.` string references.
    /// Non-expression strings and other value types are returned as-is.
    pub fn resolve_input(&self, input: &Value) -> Result<Value, ExprError> {
        match input {
            Value::String(s) if s.starts_with("$.") => self.resolve(s),
            Value::Object(map) => {
                let mut result = serde_json::Map::new();
                for (k, v) in map {
                    result.insert(k.clone(), self.resolve_input(v)?);
                }
                Ok(Value::Object(result))
            }
            Value::Array(arr) => {
                let mut result = Vec::new();
                for v in arr {
                    result.push(self.resolve_input(v)?);
                }
                Ok(Value::Array(result))
            }
            other => Ok(other.clone()),
        }
    }

    /// Evaluate a `when` condition expression against stored data.
    pub fn eval_condition(&self, condition: &str) -> Result<bool, ExprError> {
        let parsed = expr::parse_expr(condition)?;
        let result = expr::eval(&parsed, &|segments| self.resolve_segments(segments))?;
        Ok(match result {
            Value::Bool(b) => b,
            Value::Null => false,
            _ => true,
        })
    }

    /// Return all stored data.
    pub fn into_data(self) -> HashMap<String, Value> {
        self.data
    }
}

impl Default for Accumulator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn set_and_resolve() {
        let mut acc = Accumulator::new();
        acc.set("input", json!({ "email": "alice@example.com", "age": 25 }));
        acc.set("find-user", json!({ "id": 42, "name": "Alice" }));

        assert_eq!(
            acc.resolve("$.input.email").unwrap(),
            json!("alice@example.com")
        );
        assert_eq!(acc.resolve("$.find-user.id").unwrap(), json!(42));
    }

    #[test]
    fn resolve_nested() {
        let mut acc = Accumulator::new();
        acc.set(
            "step1",
            json!({ "data": { "nested": { "value": true } } }),
        );
        assert_eq!(
            acc.resolve("$.step1.data.nested.value").unwrap(),
            json!(true)
        );
    }

    #[test]
    fn resolve_array_index() {
        let mut acc = Accumulator::new();
        acc.set("step1", json!({ "items": ["a", "b", "c"] }));
        assert_eq!(acc.resolve("$.step1.items.1").unwrap(), json!("b"));
    }

    #[test]
    fn resolve_input_mixed() {
        let mut acc = Accumulator::new();
        acc.set("input", json!({ "name": "Alice" }));
        acc.set("step1", json!({ "token": "abc123" }));

        let input = json!({
            "user": "$.input.name",
            "token": "$.step1.token",
            "static_value": 42,
            "nested": {
                "ref": "$.input.name"
            }
        });

        let resolved = acc.resolve_input(&input).unwrap();
        assert_eq!(
            resolved,
            json!({
                "user": "Alice",
                "token": "abc123",
                "static_value": 42,
                "nested": {
                    "ref": "Alice"
                }
            })
        );
    }

    #[test]
    fn eval_condition_true() {
        let mut acc = Accumulator::new();
        acc.set("check", json!({ "match": true }));
        assert!(acc.eval_condition("$.check.match == true").unwrap());
    }

    #[test]
    fn eval_condition_false() {
        let mut acc = Accumulator::new();
        acc.set("check", json!({ "match": false }));
        assert!(!acc.eval_condition("$.check.match == true").unwrap());
    }

    #[test]
    fn eval_condition_numeric() {
        let mut acc = Accumulator::new();
        acc.set("step", json!({ "count": 10 }));
        assert!(acc.eval_condition("$.step.count > 5").unwrap());
        assert!(!acc.eval_condition("$.step.count < 5").unwrap());
    }

    #[test]
    fn unresolved_reference() {
        let acc = Accumulator::new();
        assert!(acc.resolve("$.nonexistent.field").is_err());
    }
}
