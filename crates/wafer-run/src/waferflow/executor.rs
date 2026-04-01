use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use wafer_flow::{Accumulator, WaferFlow};

use crate::config::parse_config_map;
use crate::observability::ObservabilityContext;
use crate::platform::Instant;
use crate::runtime::{run_block_with_recovery, Wafer};
use crate::types::*;

/// Execute a WaferFlow definition.
///
/// Two modes per step:
/// - **Middleware mode** (step has no `input`): pass `msg` through directly.
///   Short-circuit on Respond/Drop/Error.
/// - **Data pipeline mode** (step has `input`): resolve from accumulator into
///   `msg.data`, store output in accumulator.
pub async fn execute(
    flow: &WaferFlow,
    msg: &mut Message,
    wafer: &Wafer,
    cancelled: &Arc<AtomicBool>,
    deadline: Option<Instant>,
) -> Result_ {
    let max_steps = flow
        .config
        .as_ref()
        .and_then(|c| c.max_steps)
        .unwrap_or(1000) as usize;

    let on_error = flow
        .config
        .as_ref()
        .and_then(|c| c.on_error.as_deref())
        .unwrap_or("stop");

    let mut acc = Accumulator::new();

    // Seed accumulator with initial message data for pipeline mode
    let has_pipeline_steps = flow.steps.iter().any(|s| s.input.is_some());
    if has_pipeline_steps {
        let input_val: serde_json::Value =
            serde_json::from_slice(&msg.data).unwrap_or(serde_json::Value::Null);
        acc.set("input", input_val);
    }

    let steps = &flow.steps;
    let mut current = 0;
    let mut step_count = 0;
    let mut routed = false;

    while current < steps.len() {
        if step_count >= max_steps {
            return Result_ {
                action: Action::Error,
                error: Some(WaferError::new(
                    "deadline_exceeded",
                    format!("max steps ({max_steps}) exceeded in flow '{}'", flow.id),
                )),
                response: None,
                message: Some(msg.clone()),
            };
        }
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            return Result_ {
                action: Action::Error,
                error: Some(WaferError::new("cancelled", "flow cancelled")),
                response: None,
                message: Some(msg.clone()),
            };
        }
        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                return Result_ {
                    action: Action::Error,
                    error: Some(WaferError::new(
                        "deadline_exceeded",
                        format!("flow '{}' timed out", flow.id),
                    )),
                    response: None,
                    message: Some(msg.clone()),
                };
            }
        }

        let step = &steps[current];
        step_count += 1;

        // --- Resolve input (data pipeline mode) ---
        let is_pipeline = step.input.is_some();
        if is_pipeline {
            if let Some(input_template) = &step.input {
                let resolved = acc.resolve_input(input_template).map_err(|e| e.to_string());
                match resolved {
                    Ok(val) => match serde_json::to_vec(&val) {
                        Ok(data) => msg.data = data,
                        Err(e) => {
                            return Result_ {
                                action: Action::Error,
                                error: Some(WaferError::new(
                                    "internal",
                                    format!(
                                        "failed to serialize input for step '{}': {}",
                                        step.id, e
                                    ),
                                )),
                                response: None,
                                message: Some(msg.clone()),
                            };
                        }
                    },
                    Err(e) => {
                        return Result_ {
                            action: Action::Error,
                            error: Some(WaferError::new(
                                "invalid_argument",
                                format!("input resolution failed in step '{}': {}", step.id, e),
                            )),
                            response: None,
                            message: Some(msg.clone()),
                        };
                    }
                }
            }
        }

        // --- Build RuntimeContext with step config ---
        let step_config = step
            .config
            .as_ref()
            .map(parse_config_map)
            .unwrap_or_default();
        let ctx = wafer.make_context(&flow.id, &step.id, step_config, cancelled.clone(), deadline);

        // --- Look up block ---
        let block_name = wafer
            .aliases
            .get(&step.block)
            .cloned()
            .unwrap_or_else(|| step.block.clone());
        let block = match wafer
            .all_blocks
            .get(&block_name)
            .or_else(|| wafer.all_blocks.get(&step.block))
        {
            Some(b) => b.clone(),
            None => {
                return Result_ {
                    action: Action::Error,
                    error: Some(WaferError::new(
                        "not_found",
                        format!("block '{}' not found in step '{}'", step.block, step.id),
                    )),
                    response: None,
                    message: Some(msg.clone()),
                };
            }
        };

        // --- Observability: block start ---
        let obs_ctx = ObservabilityContext {
            flow_id: flow.id.clone(),
            node_path: step.id.clone(),
            block_name: step.block.clone(),
            trace_id: msg.get_meta("trace_id").to_string(),
            message: Some(msg.clone()),
        };
        wafer.hooks.fire_block_start(&obs_ctx);
        let start = Instant::now();

        // --- Execute block ---
        let result = run_block_with_recovery(&*block, &ctx, msg).await;

        // --- Observability: block end ---
        wafer
            .hooks
            .fire_block_end(&obs_ctx, &result, start.elapsed());

        // --- Process result ---
        match result.action {
            Action::Respond | Action::Drop => return result,
            Action::Error => {
                if on_error == "stop" {
                    return result;
                }
                // on_error=continue: fall through
            }
            Action::Continue => {}
        }

        // Update message from result
        if let Some(ref result_msg) = result.message {
            *msg = result_msg.clone();
        }

        // Store in accumulator for pipeline mode
        if is_pipeline {
            let output: serde_json::Value =
                serde_json::from_slice(&msg.data).unwrap_or(serde_json::Value::Null);
            acc.set(&step.id, output);
        }

        // --- Advance ---
        if let Some(next_entries) = &step.next {
            let mut jumped = false;
            for entry in next_entries {
                let should_take = match &entry.when {
                    Some(condition) => acc.eval_condition(condition).unwrap_or(false),
                    None => true,
                };
                if should_take {
                    if let Some(target_step) = &entry.step {
                        match steps.iter().position(|s| s.id == *target_step) {
                            Some(idx) => {
                                current = idx;
                                jumped = true;
                                routed = true;
                            }
                            None => {
                                return Result_ {
                                    action: Action::Error,
                                    error: Some(WaferError::new(
                                        "not_found",
                                        format!("next target step '{}' not found", target_step),
                                    )),
                                    response: None,
                                    message: Some(msg.clone()),
                                };
                            }
                        }
                    } else if let Some(target_flow) = &entry.flow {
                        // Flow transfer: execute the target flow (boxed to break recursion)
                        let flow_result = Box::pin(wafer.run(target_flow, msg)).await;
                        return flow_result;
                    }
                    break;
                }
            }
            if !jumped {
                current += 1;
                routed = false;
            }
        } else if routed {
            break;
        } else {
            current += 1;
        }
    }

    // Terminal result
    if step_count == 0 {
        // Empty flow
        return Result_::continue_with(msg.clone());
    }

    Result_::continue_with(msg.clone())
}
