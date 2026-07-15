//! WaferFlow executor: walks a flow definition and dispatches each step to
//! its block, including `each` (per-item fan-out) and `parallel`
//! (concurrent branches) composition.
//!
//! # Step semantics
//!
//! For every step, in order:
//!
//! 1. **`parallel`** (if present): each branch's steps run sequentially
//!    against a *snapshot* of the accumulator; branches run concurrently
//!    with each other. After all branches complete, every accumulator entry
//!    a branch wrote is merged back (step ids are validated unique across
//!    branches, so the merge is conflict-free). Branches cannot see sibling
//!    outputs — only state from before the parallel step plus their own.
//!    Error rule: **all branches are awaited to completion, then the first
//!    failure in branch declaration order wins** — completion timing never
//!    influences which error is reported. Branch-local `Message` mutations
//!    and bodies are discarded at the join; only accumulator entries merge.
//!    `next` routing inside a branch step is rejected with an error (branch
//!    steps run strictly in order).
//! 2. **`each`** (if present): the expression is resolved against the
//!    accumulator and must yield an array. The step's block then runs once
//!    per item, sequentially in input order, with `$.each.item` and
//!    `$.each.index` bound for the duration of the iteration. A failing
//!    item fails the step fail-fast (later items are not invoked) under
//!    `on_error = "stop"`; other `on_error` modes record `null` for the
//!    failed item and continue. Without an `input` template the item itself
//!    is the block's input body. The step's accumulator entry (and the
//!    pipeline body) is the array of per-item outputs; the transient `each`
//!    binding is removed afterwards.
//! 3. Otherwise the step's block runs once: data-pipeline mode when the
//!    step has an `input` template (output recorded in the accumulator),
//!    middleware mode when it doesn't (message passes through).
//!
//! `config.max_steps` is charged once per step visit plus once per fan-out
//! item, shared across concurrent branches.

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use wafer_block::{
    config::parse_config_map,
    core_types::*,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
};
use wafer_flow::{types::ParallelBranch, Accumulator, Step, WaferFlow};

use crate::{
    platform::{BoxFuture, Instant},
    runtime::Wafer,
};

/// Immutable per-execution context shared by every step (and every parallel
/// branch) of one flow run.
struct StepEnv<'a> {
    flow: &'a WaferFlow,
    wafer: &'a Wafer,
    cancelled: &'a Arc<AtomicBool>,
    deadline: Option<Instant>,
    on_error: &'a str,
    max_steps: usize,
    /// Step-budget counter, shared across concurrent branches.
    steps_used: AtomicUsize,
}

/// Mutable execution state owned by one sequential strand of the flow (the
/// main step list, or one parallel branch).
struct ExecState {
    acc: Accumulator,
    body: Vec<u8>,
    msg: Message,
}

/// How a single block invocation concluded (when it did not short-circuit
/// the flow).
enum InvocationOutcome {
    /// The block produced a `Response`; `ExecState::body` holds it.
    Responded,
    /// The block was middleware (`Continue`) or errored under a non-`stop`
    /// `on_error` policy; `ExecState::body` holds the restored input.
    NoOutput,
}

/// Execute a WaferFlow definition.
///
/// Each step receives the previous step's output as its input (data pipeline mode
/// when the step has an `input` template) or passes the message through (middleware
/// mode when no `input` is specified). Steps carrying `each` fan out over an
/// array; steps carrying `parallel` run their branches concurrently first —
/// see the module docs for the precise semantics.
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
    let uses_accumulator = steps_use_accumulator(&flow.steps);
    let body: Vec<u8> = if uses_accumulator {
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

    let env = StepEnv {
        flow,
        wafer,
        cancelled,
        deadline,
        on_error,
        max_steps,
        steps_used: AtomicUsize::new(0),
    };
    let mut state = ExecState { acc, body, msg };

    let steps = &flow.steps;
    let mut current = 0;
    let mut routed = false;

    while current < steps.len() {
        let step = &steps[current];

        if let Err(short_circuit) = run_step(&env, step, &mut state).await {
            return short_circuit;
        }

        // --- Advance ---
        if let Some(next_entries) = &step.next {
            let mut jumped = false;
            for entry in next_entries {
                let should_take = entry
                    .when
                    .as_ref()
                    .is_none_or(|condition| state.acc.eval_condition(condition).unwrap_or(false));
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
                                    ErrorCode::NotFound,
                                    format!("next target step '{target_step}' not found"),
                                ));
                            }
                        }
                    } else if let Some(target_flow) = &entry.flow {
                        // Flow transfer: execute the target flow (boxed to break recursion)
                        let flow_result = Box::pin(wafer.run(
                            target_flow,
                            state.msg,
                            InputStream::from_bytes(state.body),
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
    let resp_meta: Vec<MetaEntry> = state
        .msg
        .meta
        .iter()
        .filter(|e| e.key.starts_with("resp."))
        .cloned()
        .collect();
    OutputStream::respond_with_meta(state.body, resp_meta)
}

/// True if any step (recursively through parallel branches) reads from or
/// writes to the accumulator, in which case the flow input must be parsed
/// and stored under `$.input`.
fn steps_use_accumulator(steps: &[Step]) -> bool {
    steps.iter().any(|s| {
        s.input.is_some()
            || s.each.is_some()
            || s.parallel
                .as_ref()
                .is_some_and(|branches| branches.iter().any(|b| steps_use_accumulator(&b.steps)))
    })
}

/// Charge one unit of the shared step budget; error once it is exhausted.
fn check_budget(env: &StepEnv<'_>) -> Result<(), OutputStream> {
    if env.steps_used.fetch_add(1, Ordering::Relaxed) >= env.max_steps {
        return Err(OutputStream::error(WaferError::new(
            ErrorCode::ResourceExhausted,
            format!(
                "max steps ({}) exceeded in flow '{}'",
                env.max_steps, env.flow.id
            ),
        )));
    }
    Ok(())
}

/// Fail fast when the flow has been cancelled or its deadline has passed.
fn check_cancel_deadline(env: &StepEnv<'_>) -> Result<(), OutputStream> {
    if env.cancelled.load(Ordering::Relaxed) {
        return Err(OutputStream::error(WaferError::new(
            ErrorCode::Cancelled,
            "flow cancelled",
        )));
    }
    if let Some(dl) = env.deadline {
        if Instant::now() >= dl {
            env.cancelled.store(true, Ordering::Relaxed);
            return Err(OutputStream::error(WaferError::new(
                ErrorCode::DeadlineExceeded,
                format!("flow '{}' timed out", env.flow.id),
            )));
        }
    }
    Ok(())
}

/// Execute one step: parallel branches first (if any), then the step's own
/// block — fanned out per item when `each` is present. `Err` carries the
/// stream that short-circuits the whole flow.
///
/// Returns a boxed future ([`BoxFuture`]) to break the async-recursion cycle
/// step → parallel branch → step at the signature level.
fn run_step<'a>(
    env: &'a StepEnv<'a>,
    step: &'a Step,
    state: &'a mut ExecState,
) -> BoxFuture<'a, Result<(), OutputStream>> {
    Box::pin(async move {
        check_budget(env)?;
        check_cancel_deadline(env)?;

        if let Some(branches) = &step.parallel {
            run_parallel(env, branches, state).await?;
        }

        if let Some(each_path) = &step.each {
            run_each(env, step, each_path, state).await
        } else {
            let is_pipeline = step.input.is_some();
            let outcome = run_invocation(env, step, state).await?;
            if is_pipeline {
                let output = match outcome {
                    InvocationOutcome::Responded => {
                        serde_json::from_slice(&state.body).unwrap_or(serde_json::Value::Null)
                    }
                    InvocationOutcome::NoOutput => serde_json::Value::Null,
                };
                state.acc.set(&step.id, output);
            }
            Ok(())
        }
    })
}

/// Run a step's block once per item of the array `each_path` resolves to,
/// sequentially in input order, with `$.each.item` / `$.each.index` bound.
/// Records the ordered results array under the step id and as the body.
async fn run_each(
    env: &StepEnv<'_>,
    step: &Step,
    each_path: &str,
    state: &mut ExecState,
) -> Result<(), OutputStream> {
    let items = match state.acc.resolve(each_path) {
        Ok(serde_json::Value::Array(items)) => items,
        Ok(other) => {
            return Err(OutputStream::error(WaferError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "each expression '{each_path}' in step '{}' must resolve to an array, got {other}",
                    step.id
                ),
            )));
        }
        Err(e) => {
            return Err(OutputStream::error(WaferError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "each expression '{each_path}' in step '{}' failed to resolve: {e}",
                    step.id
                ),
            )));
        }
    };

    let mut results = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        check_budget(env)?;
        check_cancel_deadline(env)?;

        // Without an input template, the item itself is the block's input.
        if step.input.is_none() {
            match serde_json::to_vec(&item) {
                Ok(bytes) => state.body = bytes,
                Err(e) => {
                    return Err(OutputStream::error(WaferError::new(
                        ErrorCode::Internal,
                        format!(
                            "failed to serialize each item {index} for step '{}': {e}",
                            step.id
                        ),
                    )));
                }
            }
        }
        state
            .acc
            .set("each", serde_json::json!({ "item": item, "index": index }));

        let outcome = run_invocation(env, step, state).await?;
        results.push(match outcome {
            InvocationOutcome::Responded => {
                serde_json::from_slice(&state.body).unwrap_or(serde_json::Value::Null)
            }
            InvocationOutcome::NoOutput => serde_json::Value::Null,
        });
    }
    state.acc.remove("each");

    let results = serde_json::Value::Array(results);
    state.body = match serde_json::to_vec(&results) {
        Ok(bytes) => bytes,
        Err(e) => {
            return Err(OutputStream::error(WaferError::new(
                ErrorCode::Internal,
                format!("failed to serialize results of step '{}': {e}", step.id),
            )));
        }
    };
    state.acc.set(&step.id, results);
    Ok(())
}

/// Run all parallel branches of a step concurrently, each against a snapshot
/// of the current state, then merge the entries the branches wrote back into
/// the shared accumulator. All branches are awaited; on failure the first
/// failing branch in declaration order determines the step's outcome.
async fn run_parallel(
    env: &StepEnv<'_>,
    branches: &[ParallelBranch],
    state: &mut ExecState,
) -> Result<(), OutputStream> {
    let snapshot_keys: HashSet<String> = state.acc.keys().map(str::to_string).collect();

    let branch_runs = branches.iter().map(|branch| {
        let mut branch_state = ExecState {
            acc: state.acc.clone(),
            body: state.body.clone(),
            msg: state.msg.clone(),
        };
        async move {
            run_branch_steps(env, &branch.steps, &mut branch_state)
                .await
                .map(|()| branch_state.acc)
        }
    });
    let outcomes = futures::future::join_all(branch_runs).await;

    let mut branch_accs = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        // Declaration order: the first failed branch wins deterministically.
        branch_accs.push(outcome?);
    }
    for acc in branch_accs {
        for (key, value) in acc.into_data() {
            if !snapshot_keys.contains(&key) {
                state.acc.set(&key, value);
            }
        }
    }
    Ok(())
}

/// Run one branch's steps strictly in order. `next` routing is not
/// supported inside branches (jump targets are flow-global and branches join
/// unconditionally), so it is rejected explicitly rather than ignored.
async fn run_branch_steps(
    env: &StepEnv<'_>,
    steps: &[Step],
    state: &mut ExecState,
) -> Result<(), OutputStream> {
    for step in steps {
        if step.next.is_some() {
            return Err(OutputStream::error(WaferError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "step '{}': 'next' routing inside parallel branches is not supported — branch steps run strictly in order",
                    step.id
                ),
            )));
        }
        run_step(env, step, state).await?;
    }
    Ok(())
}

/// Resolve the step's input template (if any), dispatch its block, and apply
/// the resulting terminal to `state`. `Err` carries a flow short-circuit
/// (block missing, unresolvable input, Error under `on_error = "stop"`,
/// Drop, Halt, or a malformed stream).
async fn run_invocation(
    env: &StepEnv<'_>,
    step: &Step,
    state: &mut ExecState,
) -> Result<InvocationOutcome, OutputStream> {
    check_cancel_deadline(env)?;

    // --- Resolve input (data pipeline mode) ---
    if let Some(input_template) = &step.input {
        match state.acc.resolve_input(input_template) {
            Ok(val) => match serde_json::to_vec(&val) {
                Ok(data) => state.body = data,
                Err(e) => {
                    return Err(OutputStream::error(WaferError::new(
                        ErrorCode::Internal,
                        format!("failed to serialize input for step '{}': {}", step.id, e),
                    )));
                }
            },
            Err(e) => {
                return Err(OutputStream::error(WaferError::new(
                    ErrorCode::InvalidArgument,
                    format!("input resolution failed in step '{}': {}", step.id, e),
                )));
            }
        }
    }

    // --- Resolve the block (alias → target) first so the RuntimeContext
    //     carries the *block* identity, not the flow's step id. WRAP keys
    //     access decisions off `node_id` and the resource owner is
    //     `{org}/{block}`; passing `step.id` here would attribute all WRAP
    //     calls to the (arbitrary) step name and cause false denials.
    //     `lookup_block` is the single canonicalize-then-fallback accessor
    //     shared with the runner and `RuntimeContext` dispatch. ---
    let (block_name, block) = match env.wafer.lookup_block(&step.block) {
        Some((resolved, block)) => (resolved.to_string(), block),
        None => {
            return Err(OutputStream::error(WaferError::new(
                ErrorCode::NotFound,
                format!("block '{}' not found in step '{}'", step.block, step.id),
            )));
        }
    };

    // --- Build RuntimeContext with step config ---
    let step_config = step
        .config
        .as_ref()
        .map(parse_config_map)
        .unwrap_or_default();
    // Each flow step gets its own init stack frame for cycle detection.
    let step_init_stack = crate::runtime::init_stack::InitStack::new();
    // SEC-04: build the step context through `make_block_context` so the
    // step block's declared `requires` allowlist is enforced on `call_block`,
    // identically to top-level dispatch. `make_context` alone would leave the
    // step unrestricted.
    let ctx = env.wafer.make_block_context(
        &env.flow.id,
        &block_name,
        step_config,
        env.cancelled.clone(),
        env.deadline,
        step_init_stack.clone(),
    );

    // --- Execute block (lazy init + observability via the shared
    //     dispatch scaffold, panic recovery via run_block_with_recovery,
    //     stream collection inside the observed window). Init failure
    //     surfaces as an error event so the flow short-circuits via the
    //     standard error path. ---
    // Save body before handing it to the block — middleware (Continue)
    // blocks don't produce a response body, so we need to restore the
    // original input for the next step.
    let saved_body = state.body.clone();
    let step_input = InputStream::from_bytes(std::mem::take(&mut state.body));
    let scaffold_result = crate::runtime::runner::run_resolved(
        &env.wafer.hooks,
        crate::runtime::runner::DispatchObs {
            flow_id: &env.flow.id,
            node_path: &step.id,
            block_name: &step.block,
        },
        &block_name,
        env.wafer
            .dispatch_init(&block_name, &block, &step_init_stack),
        state.msg.clone(),
        step_input,
        |msg, input| async {
            crate::runtime::run_block_with_recovery(block.as_ref(), &ctx, msg, input)
                .await
                .collect_buffered()
                .await
        },
    )
    .await;
    let buf = match scaffold_result {
        Ok(buf) => buf,
        Err(init_failure) => return Err(init_failure),
    };

    // --- Process result ---
    match buf {
        Ok(response) => {
            state.body = response.body;

            // Apply trailing meta to the message
            for entry in response.meta {
                state.msg.set_meta(entry.key, entry.value);
            }
            Ok(InvocationOutcome::Responded)
        }
        Err(TerminalNotResponse::Error(e)) => {
            if env.on_error == "stop" {
                return Err(OutputStream::error(e));
            }
            // on_error=continue: clear body, fall through
            state.body = Vec::new();
            Ok(InvocationOutcome::NoOutput)
        }
        Err(TerminalNotResponse::Drop) => {
            // Short-circuit: block requested drop
            Err(OutputStream::drop_request())
        }
        Err(TerminalNotResponse::Halt(buf)) => {
            // Short-circuit: block produced a response and requests halt.
            // Forward the buffered response as a Halt terminal so the
            // HTTP listener can serve it while preserving the signal.
            Err(OutputStream::from_buffered_response(buf))
        }
        Err(TerminalNotResponse::Continue(next_msg)) => {
            // Middleware block — update message but restore the original
            // body so the next step receives it (the block didn't consume
            // the input, but InputStream::from_bytes took ownership).
            state.msg = next_msg;
            state.body = saved_body;
            Ok(InvocationOutcome::NoOutput)
        }
        Err(TerminalNotResponse::Malformed) => Err(OutputStream::error(WaferError::new(
            ErrorCode::Internal,
            format!(
                "block '{}' in step '{}' produced malformed output stream",
                step.block, step.id
            ),
        ))),
    }
}
