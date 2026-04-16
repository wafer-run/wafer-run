use std::sync::{atomic::AtomicBool, Arc};

use wafer_block::streams::{
    input::InputStream,
    output::{OutputStream, TerminalNotResponse},
};
use wafer_flow::{Accumulator, WaferFlow};

use crate::{
    config::parse_config_map, observability::ObservabilityContext, platform::Instant,
    runtime::Wafer, types::*,
};

/// Execute a WaferFlow definition.
///
/// Each step receives the previous step's output as its input (data pipeline mode
/// when the step has an `input` template) or passes the message through (middleware
/// mode when no `input` is specified).
///
/// Short-circuits on Error or Drop terminals from any step.
pub async fn execute(
    flow: &WaferFlow,
    msg: Message,
    input: InputStream,
    wafer: &Wafer,
    cancelled: &Arc<AtomicBool>,
    deadline: Option<Instant>,
) -> OutputStream {
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

    // Collect initial input bytes for pipeline mode
    let has_pipeline_steps = flow.steps.iter().any(|s| s.input.is_some());
    let mut current_body: Vec<u8> = if has_pipeline_steps {
        let bytes = input.collect_to_bytes().await;
        let input_val = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "flow input is not valid JSON, defaulting to null");
                serde_json::Value::Null
            }
        };
        acc.set("input", input_val);
        bytes
    } else {
        // In middleware mode we still need to pass the input to the first step
        input.collect_to_bytes().await
    };

    let mut current_msg = msg;

    let steps = &flow.steps;
    let mut current = 0;
    let mut step_count = 0;
    let mut routed = false;

    while current < steps.len() {
        if step_count >= max_steps {
            return OutputStream::error(WaferError::new(
                ErrorCode::RESOURCE_EXHAUSTED,
                format!("max steps ({max_steps}) exceeded in flow '{}'", flow.id),
            ));
        }
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            return OutputStream::error(WaferError::new(ErrorCode::CANCELLED, "flow cancelled"));
        }
        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                return OutputStream::error(WaferError::new(
                    ErrorCode::DEADLINE_EXCEEDED,
                    format!("flow '{}' timed out", flow.id),
                ));
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
                        Ok(data) => current_body = data,
                        Err(e) => {
                            return OutputStream::error(WaferError::new(
                                ErrorCode::INTERNAL,
                                format!("failed to serialize input for step '{}': {}", step.id, e),
                            ));
                        }
                    },
                    Err(e) => {
                        return OutputStream::error(WaferError::new(
                            ErrorCode::INVALID_ARGUMENT,
                            format!("input resolution failed in step '{}': {}", step.id, e),
                        ));
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
                return OutputStream::error(WaferError::new(
                    ErrorCode::NOT_FOUND,
                    format!("block '{}' not found in step '{}'", step.block, step.id),
                ));
            }
        };

        // --- Observability: block start ---
        let obs_ctx = ObservabilityContext {
            flow_id: flow.id.clone(),
            node_path: step.id.clone(),
            block_name: step.block.clone(),
            trace_id: current_msg.get_meta("trace_id").to_string(),
            message: Some(current_msg.clone()),
        };
        wafer.hooks.fire_block_start(&obs_ctx);
        let start = Instant::now();

        // --- Execute block with panic recovery ---
        // Save body before handing it to the block — middleware (Continue)
        // blocks don't produce a response body, so we need to restore the
        // original input for the next step.
        let saved_body = current_body.clone();
        let step_input = InputStream::from_bytes(std::mem::take(&mut current_body));
        let out = crate::runtime::run_block_with_recovery(
            block.as_ref(),
            &ctx,
            current_msg.clone(),
            step_input,
        )
        .await;

        // --- Collect result ---
        let buf = out.collect_buffered().await;

        // --- Observability: block end ---
        wafer.hooks.fire_block_end(&obs_ctx, start.elapsed());

        // --- Process result ---
        match buf {
            Ok(response) => {
                current_body = response.body;

                // Apply trailing meta to the message
                for entry in response.meta {
                    current_msg.set_meta(entry.key, entry.value);
                }

                // Store in accumulator for pipeline mode
                if is_pipeline {
                    let output: serde_json::Value =
                        serde_json::from_slice(&current_body).unwrap_or(serde_json::Value::Null);
                    acc.set(&step.id, output);
                }
            }
            Err(TerminalNotResponse::Error(e)) => {
                if on_error == "stop" {
                    return OutputStream::error(e);
                }
                // on_error=continue: clear body, fall through
                current_body = Vec::new();
                if is_pipeline {
                    acc.set(&step.id, serde_json::Value::Null);
                }
            }
            Err(TerminalNotResponse::Drop) => {
                // Short-circuit: block requested drop
                return OutputStream::drop_request();
            }
            Err(TerminalNotResponse::Continue(next_msg)) => {
                // Middleware block — update message but restore the original
                // body so the next step receives it (the block didn't consume
                // the input, but InputStream::from_bytes took ownership).
                current_msg = next_msg;
                current_body = saved_body;
                if is_pipeline {
                    acc.set(&step.id, serde_json::Value::Null);
                }
            }
            Err(TerminalNotResponse::Malformed) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!(
                        "block '{}' in step '{}' produced malformed output stream",
                        step.block, step.id
                    ),
                ));
            }
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
                                return OutputStream::error(WaferError::new(
                                    ErrorCode::NOT_FOUND,
                                    format!("next target step '{target_step}' not found"),
                                ));
                            }
                        }
                    } else if let Some(target_flow) = &entry.flow {
                        // Flow transfer: execute the target flow (boxed to break recursion)
                        let flow_result = Box::pin(wafer.run(
                            target_flow,
                            current_msg,
                            InputStream::from_bytes(current_body),
                        ))
                        .await;
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

    // Terminal result — respond with the last accumulated body.
    // Extract response meta (resp.*) from the message so the HTTP listener
    // can set content-type, status, headers, cookies, etc.
    let resp_meta: Vec<MetaEntry> = current_msg
        .meta
        .iter()
        .filter(|e| e.key.starts_with("resp."))
        .cloned()
        .collect();
    OutputStream::respond_with_meta(current_body, resp_meta)
}
