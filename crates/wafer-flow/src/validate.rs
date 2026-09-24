//! Semantic validation for parsed [`WaferFlow`] documents.

use std::collections::HashSet;

use crate::{error::ValidationError, expr, types::WaferFlow};

/// Validate a parsed [`WaferFlow`] for semantic correctness.
///
/// Checks performed: at least one step exists; step ids are unique
/// (including across `parallel` branches) and not reserved; every
/// `next.step` names a top-level step (branch steps are not jump targets);
/// no step inside a `parallel` branch has `next`; no `next` entry names both
/// a `step` and a `flow`, or neither; no `next.flow` names the flow itself;
/// every conditional `next` block has an unconditional default; every
/// `when`, `each`, and `$.`-prefixed `input` value parses as a valid
/// expression; and the config does not set both `timeout` and `timeout_ms`.
/// Returns *all* problems found, not just the first.
pub fn validate(flow: &WaferFlow) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    if flow.steps.is_empty() {
        errors.push(ValidationError::EmptyFlow);
        return Err(errors);
    }

    if let Some(config) = &flow.config {
        if config.timeout.is_some() && config.timeout_ms.is_some() {
            errors.push(ValidationError::ConflictingTimeouts);
        }
    }

    // Check every step id (branch steps included) for duplicates.
    collect_step_ids(&flow.steps, &mut HashSet::new(), &mut errors);

    // Only top-level steps are jump targets: the executor routes `next`
    // over the top-level step list.
    let jump_targets: HashSet<&str> = flow.steps.iter().map(|s| s.id.as_str()).collect();

    validate_steps(&flow.id, &flow.steps, &jump_targets, false, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn collect_step_ids(
    steps: &[crate::types::Step],
    ids: &mut HashSet<String>,
    errors: &mut Vec<ValidationError>,
) {
    for step in steps {
        // `input` (caller payload) and `each` (fan-out binding) are
        // accumulator keys owned by the runtime — a step claiming either
        // would silently clobber them at execution time.
        if step.id == "input" || step.id == "each" {
            errors.push(ValidationError::ReservedStepId(step.id.clone()));
        }
        if !ids.insert(step.id.clone()) {
            errors.push(ValidationError::DuplicateStepId(step.id.clone()));
        }
        // Recurse into parallel branches.
        if let Some(branches) = &step.parallel {
            for branch in branches {
                collect_step_ids(&branch.steps, ids, errors);
            }
        }
    }
}

fn validate_steps(
    flow_id: &str,
    steps: &[crate::types::Step],
    jump_targets: &HashSet<&str>,
    in_branch: bool,
    errors: &mut Vec<ValidationError>,
) {
    for step in steps {
        // Validate next entries.
        if let Some(next_entries) = &step.next {
            if in_branch {
                errors.push(ValidationError::NextInParallelBranch(step.id.clone()));
            }
            let mut has_default = false;
            for entry in next_entries {
                match (&entry.step, &entry.flow) {
                    (None, None) => {
                        errors.push(ValidationError::NextEntryMissingTarget(step.id.clone()));
                    }
                    (Some(_), Some(_)) => {
                        errors.push(ValidationError::NextEntryWithTwoTargets(step.id.clone()));
                    }
                    (Some(target), None) => {
                        if !jump_targets.contains(target.as_str()) {
                            errors.push(ValidationError::UnknownNextTarget {
                                from: step.id.clone(),
                                target: target.clone(),
                            });
                        }
                    }
                    (None, Some(target_flow)) => {
                        if target_flow == flow_id {
                            errors.push(ValidationError::TransferToSelf {
                                step: step.id.clone(),
                                flow: flow_id.to_string(),
                            });
                        }
                    }
                }

                // Track whether we have a default (no 'when') entry.
                if entry.when.is_none() {
                    has_default = true;
                }

                // Validate 'when' expressions.
                if let Some(when_expr) = &entry.when {
                    if let Err(e) = expr::parse_expr(when_expr) {
                        errors.push(ValidationError::InvalidExpression {
                            step: step.id.clone(),
                            reason: e.to_string(),
                        });
                    }
                }
            }

            if !has_default {
                errors.push(ValidationError::MissingDefaultNext(step.id.clone()));
            }
        }

        // Validate 'each' expression.
        if let Some(each_expr) = &step.each {
            if let Err(e) = expr::parse_path(each_expr) {
                errors.push(ValidationError::InvalidExpression {
                    step: step.id.clone(),
                    reason: e.to_string(),
                });
            }
        }

        // Validate input expressions.
        if let Some(input) = &step.input {
            validate_input_expressions(&step.id, input, errors);
        }

        // Recurse into parallel branches.
        if let Some(branches) = &step.parallel {
            for branch in branches {
                validate_steps(flow_id, &branch.steps, jump_targets, true, errors);
            }
        }
    }
}

fn validate_input_expressions(
    step_id: &str,
    value: &serde_json::Value,
    errors: &mut Vec<ValidationError>,
) {
    match value {
        serde_json::Value::String(s) if s.starts_with("$.") => {
            if let Err(e) = expr::parse_path(s) {
                errors.push(ValidationError::InvalidExpression {
                    step: step_id.to_string(),
                    reason: e.to_string(),
                });
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values() {
                validate_input_expressions(step_id, v, errors);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                validate_input_expressions(step_id, v, errors);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn valid_flow() {
        let json = r#"{
            "id": "test",
            "name": "Test",
            "version": "0.1.0",
            "steps": [
                { "id": "a", "block": "block-a" },
                { "id": "b", "block": "block-b" }
            ]
        }"#;
        let flow = parse(json).unwrap();
        assert!(validate(&flow).is_ok());
    }

    #[test]
    fn duplicate_step_ids() {
        let json = r#"{
            "id": "test",
            "name": "Test",
            "version": "0.1.0",
            "steps": [
                { "id": "a", "block": "block-a" },
                { "id": "a", "block": "block-b" }
            ]
        }"#;
        let flow = parse(json).unwrap();
        let errors = validate(&flow).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ValidationError::DuplicateStepId(id) if id == "a")));
    }

    #[test]
    fn unknown_next_target() {
        let json = r#"{
            "id": "test",
            "name": "Test",
            "version": "0.1.0",
            "steps": [
                {
                    "id": "a",
                    "block": "block-a",
                    "next": [
                        { "when": "$.a.x == true", "step": "nonexistent" },
                        { "step": "a" }
                    ]
                }
            ]
        }"#;
        let flow = parse(json).unwrap();
        let errors = validate(&flow).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e, ValidationError::UnknownNextTarget { target, .. } if target == "nonexistent")));
    }

    #[test]
    fn reserved_step_ids_rejected() {
        for reserved in ["input", "each"] {
            let json = format!(
                r#"{{
                    "id": "test",
                    "name": "Test",
                    "version": "0.1.0",
                    "steps": [
                        {{ "id": "{reserved}", "block": "block-a" }}
                    ]
                }}"#
            );
            let flow = parse(&json).unwrap();
            let errors = validate(&flow).unwrap_err();
            assert!(
                errors
                    .iter()
                    .any(|e| matches!(e, ValidationError::ReservedStepId(id) if id == reserved)),
                "step id '{reserved}' must be rejected as reserved"
            );
        }
    }

    #[test]
    fn missing_default_next() {
        let json = r#"{
            "id": "test",
            "name": "Test",
            "version": "0.1.0",
            "steps": [
                {
                    "id": "a",
                    "block": "block-a",
                    "next": [
                        { "when": "$.a.x == true", "step": "a" }
                    ]
                }
            ]
        }"#;
        let flow = parse(json).unwrap();
        let errors = validate(&flow).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ValidationError::MissingDefaultNext(_))));
    }

    fn flow_with_steps(steps: &str) -> WaferFlow {
        parse(&format!(
            r#"{{ "id": "test", "name": "Test", "version": "0.1.0", "steps": {steps} }}"#
        ))
        .unwrap()
    }

    #[test]
    fn next_step_into_a_parallel_branch_is_rejected() {
        let flow = flow_with_steps(
            r#"[
                { "id": "fan", "block": "b", "parallel": [
                    { "steps": [ { "id": "branch-step", "block": "b" } ] }
                ] },
                { "id": "route", "block": "b", "next": [ { "step": "branch-step" } ] }
            ]"#,
        );
        let errors = validate(&flow).unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::UnknownNextTarget { from, target }
                    if from == "route" && target == "branch-step"
            )),
            "{errors:?}"
        );
    }

    #[test]
    fn next_inside_a_parallel_branch_is_rejected() {
        let flow = flow_with_steps(
            r#"[
                { "id": "fan", "block": "b", "parallel": [
                    { "steps": [ { "id": "inner", "block": "b", "next": [ { "step": "fan" } ] } ] }
                ] }
            ]"#,
        );
        let errors = validate(&flow).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::NextInParallelBranch(id) if id == "inner")),
            "{errors:?}"
        );
    }

    #[test]
    fn next_entry_with_step_and_flow_is_rejected() {
        let flow = flow_with_steps(
            r#"[ { "id": "a", "block": "b", "next": [ { "step": "a", "flow": "other" } ] } ]"#,
        );
        let errors = validate(&flow).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::NextEntryWithTwoTargets(id) if id == "a")),
            "{errors:?}"
        );
    }

    #[test]
    fn transfer_to_own_flow_is_rejected() {
        let flow =
            flow_with_steps(r#"[ { "id": "a", "block": "b", "next": [ { "flow": "test" } ] } ]"#);
        let errors = validate(&flow).unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::TransferToSelf { step, flow } if step == "a" && flow == "test"
            )),
            "{errors:?}"
        );
    }

    #[test]
    fn transfer_to_another_flow_is_accepted() {
        let flow =
            flow_with_steps(r#"[ { "id": "a", "block": "b", "next": [ { "flow": "other" } ] } ]"#);
        assert!(validate(&flow).is_ok());
    }

    #[test]
    fn both_timeouts_are_rejected() {
        let flow = parse(
            r#"{ "id": "test", "name": "Test", "version": "0.1.0",
                 "steps": [ { "id": "a", "block": "b" } ],
                 "config": { "timeout": "30s", "timeout_ms": 5000 } }"#,
        )
        .unwrap();
        let errors = validate(&flow).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ConflictingTimeouts)),
            "{errors:?}"
        );
    }
}
