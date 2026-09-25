//! WaferFlow executor: walks a compiled flow plan and dispatches each step
//! to its block, including `each` (per-item fan-out) and `parallel`
//! (concurrent branches) composition.
//!
//! All per-step decisions — block resolution, config parsing, expression
//! parsing, jump-target lookup — are precomputed at seal time into a
//! [`CompiledFlow`] (see [`super::plan`], PERF-03); this module only
//! evaluates them.
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
//!    `on_error = "stop"`; under `on_error = "continue"` the failed item's
//!    result is `null` and the fan-out goes on. Without an `input` template the item itself
//!    is the block's input body. The step's accumulator entry (and the
//!    pipeline body) is the array of per-item outputs; the transient `each`
//!    binding is removed afterwards.
//! 3. Otherwise the step's block runs once: data-pipeline mode when the
//!    step has an `input` template (output recorded in the accumulator),
//!    middleware mode when it doesn't (message passes through).
//!
//! The step budget is charged once per step visit plus once per fan-out
//! item, shared across concurrent branches.
//!
//! # Flow transfer
//!
//! A `next` entry naming a `flow` hands the message, the current body and
//! the record of what responding steps wrote to that flow, which runs in
//! place of the rest of this one. The flows a request passes through form
//! one [`TransferChain`]: its step counter, cancellation flag and deadline
//! carry across every transfer, and each flow entered can only tighten them
//! — its `max_steps` caps the chain's running step count and its timeout,
//! counted from when it is entered, can only bring the deadline forward.
//! Every flow visit charges at least one step before it can transfer, so a
//! cycle of transfers ends with `ResourceExhausted` once the smallest
//! `max_steps` on it is spent. The runner drives the chain as a loop
//! ([`crate::Wafer`]'s `run_plan`), so a transfer never nests a call.
//!
//! The deadline is checked before every step and fan-out item, and handed
//! to each block's context; a block that is already running is not
//! interrupted.
//!
//! # Body ownership
//!
//! The pipeline body is an `Arc<Vec<u8>>` (PERF-03): handing it to a block
//! shares the buffer and clones the bytes lazily — only if the block
//! actually reads its input — and a middleware (`Continue`) step restores
//! the original body for the next step without ever having copied it.
//! Parallel branches snapshot the body with an `Arc` clone. Mutations
//! (pipeline outputs, `each` items) replace the `Arc` wholesale.
//!
//! # Response meta across steps
//!
//! A responding step's meta is laid over the flow message with the rules in
//! [`super::response_meta`]: a header replaces the message's header of the
//! same name (case-insensitively), `Vary` values are unioned, a security
//! header such as `X-Frame-Options` or `Content-Security-Policy` keeps the
//! stricter of the two values, and a cookie
//! replaces the message's cookie of the same name, `Path` and `Domain` —
//! never an unrelated cookie that happens to share its `resp.set_cookie.*`
//! key.
//!
//! # Short-circuit terminals keep the middleware's response headers
//!
//! A flow that stops early — a step's `Error` under `on_error = "stop"`, a
//! step's `Halt` or `Drop`, or an error the executor raises itself (budget,
//! deadline, a failing `next` condition, an unresolvable input, a missing
//! block or transfer target) — returns that terminal with the middleware's
//! response headers and cookies carried onto it. Carried: the
//! `resp.header.*` and `resp.set_cookie.*` entries on the flow message at the
//! moment the flow stopped that a middleware step (`Continue`) left there, or
//! that the flow's inbound message carried — CORS, security headers, a
//! refreshed session cookie — after any middleware overwrote or removed one.
//! Not carried:
//! - what a responding step wrote (its headers and cookies describe a
//!   response the flow discarded: a static file's year-long `Cache-Control`
//!   must not cache a 500, a login step's session cookie must not be set on a
//!   failed request), unless a later middleware rewrote the entry. Where a
//!   responder overwrote a middleware's header or cookie, the middleware's
//!   entry is carried in its place: CORS's `Vary: Origin` survives an asset
//!   step's `Vary: Accept-Encoding`, `X-Frame-Options` reverts to the
//!   security-headers value;
//! - body-describing headers (`Content-*`, `ETag`, `Last-Modified`,
//!   `Location`, `Accept-Ranges`) and `resp.status` / `resp.content_type`,
//!   whatever set them: the terminal has its own body;
//! - the stopping step's own partial output — an erroring block's streamed
//!   `Meta` events are discarded with its partial body, and its `Continue`
//!   message is never applied;
//! - a parallel branch's message changes, discarded at the join (see step
//!   semantics).
//!
//! A step whose output carries response meta no transport can send (an
//! invalid status, header name, or a header, cookie or content-type value
//! with a control or non-ASCII character — see
//! `wafer_block::http_codec::classify_response_meta`) fails with `Internal`,
//! whatever `on_error` says, before any of its meta is applied: the flow
//! stops with the middleware's headers carried as above, so a malformed
//! value never displaces a valid one.
//!
//! The terminal's own entries are laid over the carried ones with the same
//! rules as a responding step's, so the terminal wins, `Vary` is unioned and
//! a security header keeps the stricter value.
//! A flow transfer (`next` to another flow) hands the target the message and
//! the record of what responding steps wrote, so the target's boundary
//! applies the same rule; a transfer to an unknown flow errors with the
//! carried headers.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use wafer_block::{
    core_types::*,
    streams::{
        input::InputStream,
        output::{BufferedResponse, OutputStream, TerminalNotResponse},
    },
};
use wafer_flow::{Accumulator, OnError};

use super::{
    plan::{CompiledBranch, CompiledEach, CompiledFlow, CompiledStep, NextTarget},
    response_meta,
};
use crate::{
    platform::{BoxFuture, Instant},
    runtime::{runner::FlowPlan, Wafer},
};

/// The limits and counters one request's chain of flows shares (see the
/// module docs): the first flow run and every flow it transfers to.
pub(crate) struct TransferChain {
    /// Set once the deadline passes; handed to every block context.
    cancelled: Arc<AtomicBool>,
    /// Steps charged so far, across the chain and its concurrent branches.
    steps_used: AtomicUsize,
    /// The smallest `max_steps` of the flows entered so far.
    max_steps: usize,
    /// The earliest deadline of the flows entered so far.
    deadline: Option<Instant>,
}

impl TransferChain {
    /// A chain no flow has entered yet: nothing charged, no limits.
    pub(crate) fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            steps_used: AtomicUsize::new(0),
            max_steps: usize::MAX,
            deadline: None,
        }
    }

    /// Enter `flow` at `now`: tighten the chain's limits by the flow's own.
    pub(crate) fn enter(&mut self, flow: &CompiledFlow, now: Instant) {
        self.max_steps = self.max_steps.min(flow.max_steps);
        // A flow timeout is at most `wafer_flow::MAX_FLOW_TIMEOUT` (24h) by
        // construction, so adding it to the current instant cannot overflow.
        if let Some(own) = flow.timeout.map(|t| now + t) {
            self.deadline = Some(self.deadline.map_or(own, |chain| chain.min(own)));
        }
    }
}

/// How one flow of a [`TransferChain`] ended.
pub(crate) enum FlowOutcome<'w> {
    /// The chain's terminal.
    Done(OutputStream),
    /// The flow handed control to another flow, which runs next.
    Transfer(Transfer<'w>),
}

/// What a flow hands the flow it transfers to.
pub(crate) struct Transfer<'w> {
    /// The target flow's plan.
    pub(crate) plan: FlowPlan<'w>,
    /// The transferring flow's message.
    pub(crate) msg: Message,
    /// The transferring flow's current body: the target's input.
    pub(crate) body: Vec<u8>,
    /// The `msg` entries responding steps wrote (see the module docs).
    pub(crate) responder_record: response_meta::ResponderRecord,
}

/// Immutable per-execution context shared by every step (and every parallel
/// branch) of one flow run.
struct StepEnv<'a> {
    flow: &'a CompiledFlow,
    wafer: &'a Wafer,
    chain: &'a TransferChain,
}

/// Mutable execution state owned by one sequential strand of the flow (the
/// main step list, or one parallel branch).
struct ExecState {
    acc: Accumulator,
    body: Arc<Vec<u8>>,
    msg: Message,
    /// The `msg` response headers and cookies a responding step wrote and
    /// no middleware has rewritten since, each with the middleware entries
    /// it displaced — what a short-circuit terminal carries in its place
    /// (see the module docs).
    responder_record: response_meta::ResponderRecord,
}

/// How a single block invocation concluded (when it did not short-circuit
/// the flow).
enum InvocationOutcome {
    /// The block produced a `Response`; `ExecState::body` holds it.
    Responded,
    /// The block was middleware (`Continue`), and `ExecState::body` holds
    /// its input; or it errored under `on_error = "continue"`, and the body
    /// is empty.
    NoOutput,
}

/// A terminal that stops the flow before it runs out of steps. Kept typed
/// until the flow boundary so [`ShortCircuit::into_output`] can carry the
/// flow's response headers onto it (see the module docs).
enum ShortCircuit {
    /// A step failed under `on_error = "stop"`, or the executor raised an
    /// error of its own.
    Error(WaferError),
    /// A step produced a response and asked the flow to stop.
    Halt(BufferedResponse),
    /// A step dropped the request; carries the drop's response meta.
    Drop(Vec<MetaEntry>),
}

impl ShortCircuit {
    /// The flow's terminal: this short-circuit laid over the middleware
    /// response headers of `state`'s message (see the module docs).
    fn into_output(self, state: &ExecState) -> OutputStream {
        let with_carried = |own: Vec<MetaEntry>| {
            let mut meta = response_meta::carried(&state.msg.meta, &state.responder_record);
            response_meta::overlay(&mut meta, own);
            meta
        };
        match self {
            Self::Error(mut err) => {
                err.meta = with_carried(std::mem::take(&mut err.meta));
                OutputStream::error(err)
            }
            Self::Halt(buf) => OutputStream::from_buffered_response(BufferedResponse {
                body: buf.body,
                meta: with_carried(buf.meta),
            }),
            Self::Drop(meta) => OutputStream::drop_request_with_meta(with_carried(meta)),
        }
    }
}

/// Take the body buffer out of its `Arc` for a consumer that needs owned
/// bytes (flow transfer, terminal response). Zero-copy when the executor is
/// the sole holder — the common case — and a clone otherwise.
fn unwrap_body(body: Arc<Vec<u8>>) -> Vec<u8> {
    Arc::try_unwrap(body).unwrap_or_else(|arc| (*arc).clone())
}

/// Execute a compiled WaferFlow plan.
///
/// Each step receives the previous step's output as its input (data pipeline mode
/// when the step has an `input` template) or passes the message through (middleware
/// mode when no `input` is specified). Steps carrying `each` fan out over an
/// array; steps carrying `parallel` run their branches concurrently first —
/// see the module docs for the precise semantics.
///
/// Short-circuits on a step's Error (under `on_error = "stop"`), Halt or Drop
/// terminal, carrying the middleware's response headers onto it (see the
/// module docs). A taken `next` entry naming a flow ends this flow with a
/// [`FlowOutcome::Transfer`] to it. `chain` must have entered `flow`;
/// `responder_record` is the record of the `msg` entries a responding step
/// already wrote: empty for the chain's first flow, the transferring flow's
/// record after a transfer.
pub(crate) async fn execute<'w>(
    flow: &CompiledFlow,
    msg: Message,
    input: InputStream,
    wafer: &'w Wafer,
    chain: &TransferChain,
    responder_record: response_meta::ResponderRecord,
) -> FlowOutcome<'w> {
    let mut acc = Accumulator::new();

    // Collect initial input bytes; in pipeline mode also parse them into
    // the accumulator's `$.input` entry (flag precomputed at seal).
    let body = match input.collect_to_bytes().await {
        Ok(body) => body,
        Err(e) => return FlowOutcome::Done(OutputStream::error(e)),
    };
    if flow.uses_accumulator {
        let input_val = match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "flow input is not valid JSON, defaulting to null");
                serde_json::Value::Null
            }
        };
        acc.set("input", input_val);
    }

    let env = StepEnv { flow, wafer, chain };
    let mut state = ExecState {
        acc,
        body: Arc::new(body),
        msg,
        responder_record,
    };

    let steps = &flow.steps;
    let mut current = 0;
    let mut routed = false;

    while current < steps.len() {
        let step = &steps[current];

        if let Err(short_circuit) = run_step(&env, step, &mut state).await {
            return FlowOutcome::Done(short_circuit.into_output(&state));
        }

        // --- Advance ---
        if let Some(next_entries) = &step.next {
            let mut jumped = false;
            for entry in next_entries {
                let should_take = match &entry.when {
                    None => true,
                    Some((condition, compiled)) => match compiled.eval(&state.acc) {
                        Ok(taken) => taken,
                        // COR-03: a condition that fails to evaluate at runtime
                        // (a missing/undefined reference or a type error) is an
                        // authoring or data error — surface it as a typed flow
                        // error instead of silently swallowing it to `false`,
                        // which would route to a different branch and make the
                        // mistake look like a valid business decision. (An
                        // explicit JSON `null` still evaluates to `false` by
                        // design in the expression layer; only genuine
                        // evaluation errors reach here.)
                        Err(e) => {
                            return FlowOutcome::Done(
                                ShortCircuit::Error(WaferError::new(
                                    ErrorCode::InvalidArgument,
                                    format!(
                                        "flow '{}' step '{}': condition '{condition}' failed \
                                         to evaluate: {e}",
                                        flow.id, step.id
                                    ),
                                ))
                                .into_output(&state),
                            );
                        }
                    },
                };
                if should_take {
                    match &entry.target {
                        NextTarget::Step(idx) => {
                            current = *idx;
                            jumped = true;
                            routed = true;
                        }
                        NextTarget::MissingStep(target_step) => {
                            return FlowOutcome::Done(
                                ShortCircuit::Error(WaferError::new(
                                    ErrorCode::Unimplemented,
                                    format!("next target step '{target_step}' does not exist"),
                                ))
                                .into_output(&state),
                            );
                        }
                        NextTarget::Flow(target_flow) => {
                            // Flow transfer: the target runs with this flow's
                            // message and responder record, so its own
                            // boundary carries the middleware's response
                            // headers.
                            let Some(plan) = wafer.flow_plan(target_flow) else {
                                return FlowOutcome::Done(
                                    ShortCircuit::Error(crate::runtime::runner::flow_not_found(
                                        target_flow,
                                    ))
                                    .into_output(&state),
                                );
                            };
                            return FlowOutcome::Transfer(Transfer {
                                plan,
                                msg: state.msg,
                                body: unwrap_body(state.body),
                                responder_record: state.responder_record,
                            });
                        }
                        // Entry with neither `step` nor `flow`: taking it ends
                        // routing without jumping (sequential advance below).
                        NextTarget::None => {}
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
    FlowOutcome::Done(OutputStream::respond_with_meta(
        unwrap_body(state.body),
        resp_meta,
    ))
}

/// Charge one unit of the chain's step budget; error once it is exhausted.
fn check_budget(env: &StepEnv<'_>) -> Result<(), ShortCircuit> {
    if env.chain.steps_used.fetch_add(1, Ordering::Relaxed) >= env.chain.max_steps {
        return Err(ShortCircuit::Error(WaferError::new(
            ErrorCode::ResourceExhausted,
            format!(
                "max steps ({}) exceeded in flow '{}'",
                env.chain.max_steps, env.flow.id
            ),
        )));
    }
    Ok(())
}

/// Fail fast when the flow has been cancelled or its deadline has passed.
fn check_cancel_deadline(env: &StepEnv<'_>) -> Result<(), ShortCircuit> {
    if env.chain.cancelled.load(Ordering::Relaxed) {
        return Err(ShortCircuit::Error(WaferError::new(
            ErrorCode::Cancelled,
            "flow cancelled",
        )));
    }
    if let Some(dl) = env.chain.deadline {
        if Instant::now() >= dl {
            env.chain.cancelled.store(true, Ordering::Relaxed);
            return Err(ShortCircuit::Error(WaferError::new(
                ErrorCode::DeadlineExceeded,
                format!("flow '{}' timed out", env.flow.id),
            )));
        }
    }
    Ok(())
}

/// Execute one step: parallel branches first (if any), then the step's own
/// block — fanned out per item when `each` is present. `Err` carries the
/// terminal that short-circuits the whole flow.
///
/// Returns a boxed future ([`BoxFuture`]) to break the async-recursion cycle
/// step → parallel branch → step at the signature level.
fn run_step<'a>(
    env: &'a StepEnv<'a>,
    step: &'a CompiledStep,
    state: &'a mut ExecState,
) -> BoxFuture<'a, Result<(), ShortCircuit>> {
    Box::pin(async move {
        check_budget(env)?;
        check_cancel_deadline(env)?;

        if let Some(branches) = &step.parallel {
            run_parallel(env, branches, state).await?;
        }

        if let Some(each) = &step.each {
            run_each(env, step, each, state).await
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

/// Run a step's block once per item of the array `each` resolves to,
/// sequentially in input order, with `$.each.item` / `$.each.index` bound.
/// Records the ordered results array under the step id and as the body.
async fn run_each(
    env: &StepEnv<'_>,
    step: &CompiledStep,
    each: &CompiledEach,
    state: &mut ExecState,
) -> Result<(), ShortCircuit> {
    let items = match each.path.resolve(&state.acc) {
        Ok(serde_json::Value::Array(items)) => items,
        Ok(other) => {
            return Err(ShortCircuit::Error(WaferError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "each expression '{}' in step '{}' must resolve to an array, got {other}",
                    each.raw, step.id
                ),
            )));
        }
        Err(e) => {
            return Err(ShortCircuit::Error(WaferError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "each expression '{}' in step '{}' failed to resolve: {e}",
                    each.raw, step.id
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
                Ok(bytes) => state.body = Arc::new(bytes),
                Err(e) => {
                    return Err(ShortCircuit::Error(WaferError::new(
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
        Ok(bytes) => Arc::new(bytes),
        Err(e) => {
            return Err(ShortCircuit::Error(WaferError::new(
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
///
/// PERF-03: forking a branch no longer deep-copies the accumulator — the
/// pre-fork accumulator is frozen behind an `Arc` and every branch layers a
/// copy-on-write delta over it ([`Accumulator::branch_from`]); the body
/// snapshot is an `Arc` clone. The join merges exactly the branch deltas,
/// still skipping keys that existed at fork time (only unvalidated flows can
/// produce such collisions, and the deep-copy implementation discarded them
/// too).
async fn run_parallel(
    env: &StepEnv<'_>,
    branches: &[CompiledBranch],
    state: &mut ExecState,
) -> Result<(), ShortCircuit> {
    // Freeze the pre-fork accumulator. `state.acc` is left empty while the
    // branches run; it is restored from `parent` before any return below.
    let parent = Arc::new(std::mem::take(&mut state.acc));

    let branch_runs = branches.iter().map(|branch| {
        let mut branch_state = ExecState {
            acc: Accumulator::branch_from(parent.clone()),
            body: state.body.clone(),
            msg: state.msg.clone(),
            responder_record: state.responder_record.clone(),
        };
        async move {
            run_branch_steps(env, &branch.steps, &mut branch_state)
                .await
                .map(|()| branch_state.acc)
        }
    });
    let outcomes = futures::future::join_all(branch_runs).await;

    // Extract each successful branch's delta (dropping its reference to
    // `parent`) and remember the first failure in declaration order.
    let mut deltas: Vec<HashMap<String, serde_json::Value>> = Vec::with_capacity(outcomes.len());
    let mut first_failure = None;
    for outcome in outcomes {
        match outcome {
            Ok(branch_acc) => deltas.push(branch_acc.into_data()),
            Err(failure) => {
                if first_failure.is_none() {
                    first_failure = Some(failure);
                }
            }
        }
    }

    // Only keys that did not exist at fork time merge back.
    for delta in &mut deltas {
        delta.retain(|key, _| parent.get(key).is_none());
    }

    // All branch accumulators are gone, so the executor is the sole holder
    // again; restore the pre-fork accumulator without copying.
    state.acc = Arc::try_unwrap(parent).unwrap_or_else(|arc| (*arc).clone());

    if let Some(failure) = first_failure {
        return Err(failure);
    }
    for delta in deltas {
        for (key, value) in delta {
            state.acc.set(&key, value);
        }
    }
    Ok(())
}

/// Run one branch's steps strictly in order. `next` routing is not
/// supported inside branches (jump targets are flow-global and branches join
/// unconditionally), so it is rejected explicitly rather than ignored.
async fn run_branch_steps(
    env: &StepEnv<'_>,
    steps: &[CompiledStep],
    state: &mut ExecState,
) -> Result<(), ShortCircuit> {
    for step in steps {
        if step.next.is_some() {
            return Err(ShortCircuit::Error(WaferError::new(
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
    step: &CompiledStep,
    state: &mut ExecState,
) -> Result<InvocationOutcome, ShortCircuit> {
    check_cancel_deadline(env)?;

    // --- Resolve input (data pipeline mode; template compiled at seal) ---
    if let Some(template) = &step.input {
        match template.resolve(&state.acc) {
            Ok(val) => match serde_json::to_vec(&val) {
                Ok(data) => state.body = Arc::new(data),
                Err(e) => {
                    return Err(ShortCircuit::Error(WaferError::new(
                        ErrorCode::Internal,
                        format!("failed to serialize input for step '{}': {}", step.id, e),
                    )));
                }
            },
            Err(e) => {
                return Err(ShortCircuit::Error(WaferError::new(
                    ErrorCode::InvalidArgument,
                    format!("input resolution failed in step '{}': {}", step.id, e),
                )));
            }
        }
    }

    // --- Dispatch target (alias → block + slot, resolved at seal). The
    //     compiled target carries the *canonical* block identity, not the
    //     flow's step id: WRAP keys access decisions off `node_id` and the
    //     resource owner is `{org}/{block}`; passing `step.id` here would
    //     attribute all WRAP calls to the (arbitrary) step name and cause
    //     false denials. ---
    let Some(target) = &step.target else {
        return Err(ShortCircuit::Error(WaferError::new(
            ErrorCode::Unimplemented,
            format!(
                "block '{}' in step '{}' is not registered",
                step.block_label, step.id
            ),
        )));
    };

    // --- Build RuntimeContext with the step's seal-parsed config ---
    // SEC-04: build the step context through `make_block_context` so the
    // step block's declared `requires` allowlist is enforced on `call_block`,
    // identically to top-level dispatch. `make_context` alone would leave the
    // step unrestricted.
    let ctx = env.wafer.make_block_context(
        &env.flow.id,
        &target.name,
        step.config.clone(),
        env.chain.cancelled.clone(),
        env.chain.deadline,
    );

    // --- Execute block (lazy init + observability via the shared
    //     dispatch scaffold, panic recovery via run_block_with_recovery,
    //     stream collection inside the observed window). Init failure
    //     comes back as a typed error so the flow short-circuits via the
    //     standard error path. ---
    // The block gets a lazily-cloned view of the shared body: the buffer is
    // copied only if the block polls its input, and `state.body` keeps the
    // original for middleware (Continue) blocks — which don't produce a
    // response body — without an eager per-step clone.
    let shared_body = state.body.clone();
    let step_input =
        InputStream::from_stream(futures::stream::once(
            async move { Ok((*shared_body).clone()) },
        ));
    let scaffold_result = crate::runtime::runner::run_resolved(
        &env.wafer.hooks,
        crate::runtime::runner::DispatchObs {
            flow_id: &env.flow.id,
            node_path: &step.id,
            block_name: &step.block_label,
        },
        crate::runtime::runner::DispatchTarget {
            resolved: &target.name,
            slot: &target.slot,
        },
        crate::runtime::runner::DispatchInit {
            block: &target.block,
            template: &ctx,
        },
        state.msg.clone(),
        step_input,
        |msg, input| async {
            crate::runtime::run_block_with_recovery(target.block.as_ref(), &ctx, msg, input)
                .await
                .collect_buffered()
                .await
        },
    )
    .await;
    let buf = match scaffold_result {
        Ok(buf) => buf,
        Err(init_failure) => return Err(ShortCircuit::Error(init_failure)),
    };

    // --- Refuse response meta no transport can send, before any of it is
    //     laid over the flow message (see the module docs). A middleware's
    //     `Continue` is checked only for the entries it changed. ---
    let produced: Vec<&MetaEntry> = match &buf {
        Ok(response) => response.meta.iter().collect(),
        Err(TerminalNotResponse::Error(e)) => e.meta.iter().collect(),
        Err(TerminalNotResponse::Drop { meta }) => meta.iter().collect(),
        Err(TerminalNotResponse::Halt(halt)) => halt.meta.iter().collect(),
        Err(TerminalNotResponse::Continue(next)) => next
            .meta
            .iter()
            .filter(|e| !state.msg.meta.contains(e))
            .collect(),
        Err(TerminalNotResponse::Malformed) => Vec::new(),
    };
    if let Some(invalid) = response_meta::first_unsendable(produced) {
        return Err(ShortCircuit::Error(WaferError::new(
            ErrorCode::Internal,
            format!(
                "block '{}' in step '{}' produced unsendable response meta: {}",
                step.block_label, step.id, invalid
            ),
        )));
    }

    // --- Process result ---
    match buf {
        Ok(response) => {
            state.body = Arc::new(response.body);

            // Lay the response's meta over the message (see the module docs)
            // and record which headers and cookies it wrote over what.
            response_meta::apply_response(
                &mut state.responder_record,
                &mut state.msg.meta,
                response.meta,
            );
            Ok(InvocationOutcome::Responded)
        }
        Err(TerminalNotResponse::Error(e)) => match env.flow.on_error {
            OnError::Stop => Err(ShortCircuit::Error(e)),
            OnError::Continue => {
                // Clear the body and fall through to the next step.
                state.body = Arc::new(Vec::new());
                Ok(InvocationOutcome::NoOutput)
            }
        },
        Err(TerminalNotResponse::Drop { meta }) => {
            // Short-circuit: block requested drop
            Err(ShortCircuit::Drop(meta))
        }
        Err(TerminalNotResponse::Halt(buf)) => {
            // Short-circuit: block produced a response and requests halt.
            // The flow boundary forwards the buffered response as a Halt
            // terminal so the HTTP listener can serve it while preserving
            // the signal.
            Err(ShortCircuit::Halt(buf))
        }
        Err(TerminalNotResponse::Continue(next_msg)) => {
            // Middleware block — update the message. The body was never
            // taken out of `state`, so the next step sees the original
            // input with no restore copy.
            response_meta::after_continue(
                &mut state.responder_record,
                &state.msg.meta,
                &next_msg.meta,
            );
            state.msg = next_msg;
            Ok(InvocationOutcome::NoOutput)
        }
        Err(TerminalNotResponse::Malformed) => Err(ShortCircuit::Error(WaferError::new(
            ErrorCode::Internal,
            format!(
                "block '{}' in step '{}' produced malformed output stream",
                step.block_label, step.id
            ),
        ))),
    }
}
