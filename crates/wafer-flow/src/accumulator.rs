//! Runtime state for a flow execution: stores step outputs and resolves
//! `$.step-id.field` references against them.

use std::{collections::HashMap, sync::Arc};

use serde_json::Value;

use crate::{error::ExprError, expr};

/// Stores step outputs keyed by step id and resolves `$.` references and
/// `when` expressions against the stored data.
///
/// The executor creates one accumulator per flow invocation, seeds it with
/// the caller's payload under the `input` key, then calls [`set`](Self::set)
/// after every step.
///
/// # Branch layering
///
/// [`branch_from`](Self::branch_from) creates a copy-on-write child layer
/// over a frozen parent: reads fall through to the parent, writes stay in
/// the child. The executor uses this for `parallel` branches so forking a
/// branch costs one `Arc` clone instead of a deep copy of every stored
/// step output, and the branch's writes come back out as an explicit delta
/// via [`into_data`](Self::into_data).
#[derive(Debug, Clone)]
pub struct Accumulator {
    /// Frozen parent layer (only set for [`branch_from`](Self::branch_from)
    /// accumulators). Never mutated through this handle.
    parent: Option<Arc<Accumulator>>,
    data: HashMap<String, Value>,
}

impl Accumulator {
    /// Create an empty accumulator with no stored step outputs.
    pub fn new() -> Self {
        Self {
            parent: None,
            data: HashMap::new(),
        }
    }

    /// Create a copy-on-write child layer over `parent`.
    ///
    /// Reads ([`get`](Self::get), `$.` resolution, `when` evaluation) see
    /// the parent's entries; writes ([`set`](Self::set)) land in the child
    /// only and shadow same-named parent entries.
    /// [`into_data`](Self::into_data) returns only the child's writes.
    ///
    /// [`remove`](Self::remove) only affects the child layer: removing a key
    /// the *parent* also holds re-exposes the parent value rather than
    /// hiding it. The executor never does that — it only removes transient
    /// keys it set in the same layer (the `each` binding) — so no tombstone
    /// mechanism is carried for it.
    pub fn branch_from(parent: Arc<Accumulator>) -> Self {
        Self {
            parent: Some(parent),
            data: HashMap::new(),
        }
    }

    /// Store a step's output under its id (in this layer).
    pub fn set(&mut self, step_id: &str, value: Value) {
        self.data.insert(step_id.to_string(), value);
    }

    /// Get the raw value for a step id, falling through to parent layers.
    pub fn get(&self, step_id: &str) -> Option<&Value> {
        let mut cur = self;
        loop {
            if let Some(v) = cur.data.get(step_id) {
                return Some(v);
            }
            cur = cur.parent.as_deref()?;
        }
    }

    /// Remove a stored value from this layer (used by the executor to drop
    /// the transient `each` binding after a fan-out step completes). See
    /// [`branch_from`](Self::branch_from) for the layered caveat.
    pub fn remove(&mut self, step_id: &str) {
        self.data.remove(step_id);
    }

    /// Iterate over the step ids currently visible (this layer plus parent
    /// layers, deduplicated).
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        let mut seen: Vec<&str> = Vec::new();
        let mut cur = Some(self);
        while let Some(acc) = cur {
            for k in acc.data.keys() {
                let k = k.as_str();
                if !seen.contains(&k) {
                    seen.push(k);
                }
            }
            cur = acc.parent.as_deref();
        }
        seen.into_iter()
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
        let root = self
            .get(root_key)
            .ok_or_else(|| ExprError::UnresolvedReference(format!("$.{root_key}")))?;

        let mut current = root;
        // `idx` is the index of `seg` within `segments`; tracking it directly
        // avoids an O(n) `position()` rescan per traversal step.
        for (idx, seg) in segments.iter().enumerate().skip(1) {
            current = match current {
                Value::Object(map) => map.get(seg.as_str()).ok_or_else(|| {
                    ExprError::UnresolvedReference(format!("$.{}", segments[..=idx].join(".")))
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
    ///
    /// Parse-at-use twin of [`crate::compiled::CompiledTemplate`] (which is
    /// the single source of the walk semantics).
    pub fn resolve_input(&self, input: &Value) -> Result<Value, ExprError> {
        crate::compiled::CompiledTemplate::compile(input).resolve(self)
    }

    /// Evaluate a `when` condition expression against stored data.
    ///
    /// Parse-at-use twin of [`crate::compiled::CompiledCondition`] (which is
    /// the single source of the truthiness semantics).
    pub fn eval_condition(&self, condition: &str) -> Result<bool, ExprError> {
        crate::compiled::CompiledCondition::compile(condition).eval(self)
    }

    /// Return this layer's stored data. For a plain accumulator that is
    /// everything stored; for a [`branch_from`](Self::branch_from) layer it
    /// is only the branch's own writes (the delta), without parent entries.
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
    use serde_json::json;

    use super::*;

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
        acc.set("step1", json!({ "data": { "nested": { "value": true } } }));
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

    #[test]
    fn branch_layer_reads_through_to_parent() {
        let mut parent = Accumulator::new();
        parent.set("input", json!({ "v": 1 }));
        parent.set("pre", json!({ "out": "x" }));
        let parent = std::sync::Arc::new(parent);

        let branch = Accumulator::branch_from(parent);
        assert_eq!(branch.get("input"), Some(&json!({ "v": 1 })));
        assert_eq!(branch.resolve("$.pre.out").unwrap(), json!("x"));
        assert!(branch.eval_condition("$.input.v == 1").unwrap());
    }

    #[test]
    fn branch_layer_writes_stay_local_and_shadow() {
        let mut parent = Accumulator::new();
        parent.set("input", json!(1));
        let parent = std::sync::Arc::new(parent);

        let mut branch = Accumulator::branch_from(parent.clone());
        branch.set("b1", json!("branch-output"));
        branch.set("input", json!(2)); // shadow

        // Local read sees the shadow; parent is untouched.
        assert_eq!(branch.get("input"), Some(&json!(2)));
        assert_eq!(parent.get("input"), Some(&json!(1)));

        // Delta contains only the branch's writes.
        let delta = branch.into_data();
        assert_eq!(delta.len(), 2);
        assert_eq!(delta["b1"], json!("branch-output"));
        assert_eq!(delta["input"], json!(2));
    }

    #[test]
    fn branch_layer_keys_are_deduplicated_union() {
        let mut parent = Accumulator::new();
        parent.set("input", json!(1));
        parent.set("pre", json!(2));
        let mut branch = Accumulator::branch_from(std::sync::Arc::new(parent));
        branch.set("b1", json!(3));
        branch.set("pre", json!(4)); // shadow

        let mut keys: Vec<&str> = branch.keys().collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["b1", "input", "pre"]);
    }
}
