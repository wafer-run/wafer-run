use std::collections::HashSet;

use crate::error::ValidationError;
use crate::expr;
use crate::types::WaferFlow;

/// Validate a parsed WaferFlow definition for semantic correctness.
pub fn validate(flow: &WaferFlow) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    if flow.steps.is_empty() {
        errors.push(ValidationError::EmptyFlow);
        return Err(errors);
    }

    // Collect all step IDs and check for duplicates.
    let mut step_ids = HashSet::new();
    collect_step_ids(&flow.steps, &mut step_ids, &mut errors);

    // Validate next targets and expressions.
    validate_steps(&flow.steps, &step_ids, &mut errors);

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
    steps: &[crate::types::Step],
    all_ids: &HashSet<String>,
    errors: &mut Vec<ValidationError>,
) {
    for step in steps {
        // Validate next entries.
        if let Some(next_entries) = &step.next {
            let mut has_default = false;
            for entry in next_entries {
                // Each entry must have step or flow.
                if entry.step.is_none() && entry.flow.is_none() {
                    errors.push(ValidationError::NextEntryMissingTarget(step.id.clone()));
                }

                // If step target, must exist in the flow.
                if let Some(target) = &entry.step {
                    if !all_ids.contains(target) {
                        errors.push(ValidationError::UnknownNextTarget {
                            from: step.id.clone(),
                            target: target.clone(),
                        });
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
                validate_steps(&branch.steps, all_ids, errors);
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
}
