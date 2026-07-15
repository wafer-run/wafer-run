//! Criterion benchmarks for waferflow execution and native block dispatch —
//! the PERF-03 measurement prerequisite from the 2026-07-14 deep-dive review.
//!
//! PERF-03 targets per-call work that could be precomputed at seal time:
//! `run_block`'s linear snapshot search + per-call block-config reparse
//! (`runtime/runner.rs`), the per-step `step.config` reparse and the
//! middleware body clone in the flow executor (`waferflow/executor.rs`).
//!
//! Coverage:
//!
//! - `block_dispatch/run_block_native_echo` — a single `run_block` into a
//!   native echo block: the per-dispatch floor (snapshot lookup, config
//!   reparse, context construction) with negligible block work.
//! - `flow_dispatch/single_step` — smallest possible flow (one pipeline
//!   step): per-flow overhead on top of the dispatch floor.
//! - `flow_dispatch/chain_8_steps` — eight chained pipeline steps, each with
//!   an input template resolving the previous step's output and a per-step
//!   config map (exercises the per-step reparse).
//! - `flow_dispatch/middleware_8_steps` — eight middleware steps (Continue)
//!   followed by a terminal pipeline step (exercises the per-step body
//!   clone/restore).
//! - `flow_dispatch/parallel_4_branches` — a pipeline step, then a step with
//!   four parallel branches of one pipeline step each (exercises the
//!   per-branch accumulator/body/message snapshot).
//!
//! All flows run over the same ~1 KiB JSON input so per-step overhead — not
//! payload size — dominates.

use std::{hint::black_box, sync::Arc, time::Duration};

use criterion::{criterion_group, criterion_main, Criterion};
use serde_json::{json, Value};
use wafer_flow::{types::ParallelBranch, Step, WaferFlow};
use wafer_run::{
    Block, BlockInfo, Context, InputStream, InstanceMode, Message, OutputStream, Wafer,
};

// ---------------------------------------------------------------------------
// Bench blocks
// ---------------------------------------------------------------------------

/// Echoes its input bytes back (pipeline-mode step body / dispatch callee).
struct EchoBlock;

#[async_trait::async_trait]
impl Block for EchoBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "bench/native-echo",
            "0.0.1",
            "http-handler@v1",
            "echo fixture",
        )
        .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        OutputStream::respond(input.collect_to_bytes().await)
    }
}

/// Middleware-mode step body: passes the message through unchanged.
struct PassThroughBlock;

#[async_trait::async_trait]
impl Block for PassThroughBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "bench/passthrough",
            "0.0.1",
            "http-handler@v1",
            "middleware pass-through fixture",
        )
        .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::continue_with(msg)
    }
}

// ---------------------------------------------------------------------------
// Flow builders — same shapes as tests/flow_each_parallel.rs
// ---------------------------------------------------------------------------

fn make_flow(id: &str, steps: Vec<Step>) -> WaferFlow {
    WaferFlow {
        id: id.to_string(),
        name: format!("Bench flow: {id}"),
        version: "0.0.1".to_string(),
        description: None,
        input: None,
        output: None,
        steps,
        config: None,
        blocks: None,
        config_map: None,
        config_defaults: None,
    }
}

fn step(id: &str, block: &str) -> Step {
    Step {
        id: id.to_string(),
        block: block.to_string(),
        input: None,
        next: None,
        each: None,
        parallel: None,
        description: None,
        config: None,
    }
}

fn pipeline_step(id: &str, block: &str, input: Value) -> Step {
    Step {
        input: Some(input),
        // Per-step config map — reparsed on every flow invocation today
        // (waferflow/executor.rs), which is what this bench captures.
        config: Some(json!({ "bench_key": "bench_value" })),
        ..step(id, block)
    }
}

/// One pipeline step reading the flow input.
fn single_step_flow() -> WaferFlow {
    make_flow(
        "bench-single-step",
        vec![pipeline_step(
            "s0",
            "bench/native-echo",
            json!({ "v": "$.input.v" }),
        )],
    )
}

/// Eight chained pipeline steps; each resolves the previous step's output.
fn chain_flow(steps: usize) -> WaferFlow {
    let mut flow_steps = Vec::with_capacity(steps);
    for i in 0..steps {
        let input = if i == 0 {
            json!({ "v": "$.input.v" })
        } else {
            json!({ "v": format!("$.s{}.v", i - 1) })
        };
        flow_steps.push(pipeline_step(&format!("s{i}"), "bench/native-echo", input));
    }
    make_flow("bench-chain", flow_steps)
}

/// Eight middleware (Continue) steps, then a terminal pipeline step that
/// responds — the middleware steps exercise the per-step body clone/restore.
fn middleware_flow(middleware_steps: usize) -> WaferFlow {
    let mut flow_steps = Vec::with_capacity(middleware_steps + 1);
    for i in 0..middleware_steps {
        flow_steps.push(step(&format!("m{i}"), "bench/passthrough"));
    }
    flow_steps.push(pipeline_step(
        "terminal",
        "bench/native-echo",
        json!({ "v": "$.input.v" }),
    ));
    make_flow("bench-middleware", flow_steps)
}

/// One pipeline step feeding a step with `branches` parallel branches (each a
/// single pipeline step reading the pre-fork output) — every branch snapshots
/// the accumulator, body, and message, which is what this bench captures. The
/// host step's own invocation joins on one branch output so the final
/// response equals the flow input.
fn parallel_flow(branches: usize) -> WaferFlow {
    let branch_defs = (0..branches)
        .map(|i| ParallelBranch {
            steps: vec![pipeline_step(
                &format!("b{i}"),
                "bench/native-echo",
                json!({ "v": "$.pre.v" }),
            )],
        })
        .collect();
    let join = Step {
        parallel: Some(branch_defs),
        ..pipeline_step("join", "bench/native-echo", json!({ "v": "$.b0.v" }))
    };
    make_flow(
        "bench-parallel",
        vec![
            pipeline_step("pre", "bench/native-echo", json!({ "v": "$.input.v" })),
            join,
        ],
    )
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn tokio_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds")
}

async fn sealed_wafer(flows: Vec<WaferFlow>) -> Wafer {
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    w.register_block("bench/native-echo", Arc::new(EchoBlock))
        .expect("register echo");
    w.register_block("bench/passthrough", Arc::new(PassThroughBlock))
        .expect("register passthrough");
    for flow in flows {
        w.add_flow(flow);
    }
    w.seal().await.expect("seal");
    w
}

/// Run a flow to a buffered response body; panics on a non-Respond terminal
/// so mis-setup fails loudly.
async fn run_flow_once(w: &Wafer, flow_id: &str, body: &[u8]) -> Vec<u8> {
    let out = w
        .run(
            flow_id,
            Message::new("http.request"),
            InputStream::from_bytes(body.to_vec()),
        )
        .await;
    out.collect_buffered()
        .await
        .unwrap_or_else(|t| panic!("flow {flow_id} should Respond, got: {t:?}"))
        .body
}

/// Run a native block via `run_block` to a buffered response body.
async fn run_block_once(w: &Wafer, block: &str, body: &[u8]) -> Vec<u8> {
    let out = w
        .run_block(
            block,
            Message::new("bench.echo"),
            InputStream::from_bytes(body.to_vec()),
        )
        .await;
    out.collect_buffered()
        .await
        .unwrap_or_else(|t| panic!("run_block({block}) should Respond, got: {t:?}"))
        .body
}

/// ~1 KiB JSON flow input: `{"v": "aaa…"}`.
fn flow_input() -> Vec<u8> {
    serde_json::to_vec(&json!({ "v": "a".repeat(1024) })).expect("input serializes")
}

// ---------------------------------------------------------------------------
// Benches
// ---------------------------------------------------------------------------

fn bench_block_dispatch(c: &mut Criterion) {
    let rt = tokio_rt();
    let wafer = rt.block_on(sealed_wafer(Vec::new()));
    let body = flow_input();

    // Sanity-check once before measuring.
    let echoed = rt.block_on(run_block_once(&wafer, "bench/native-echo", &body));
    assert_eq!(echoed, body, "run_block echo mismatch");

    let mut group = c.benchmark_group("block_dispatch");
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("run_block_native_echo", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(run_block_once(&wafer, "bench/native-echo", &body).await) })
    });
    group.finish();
}

fn bench_flow_dispatch(c: &mut Criterion) {
    let rt = tokio_rt();
    let wafer = rt.block_on(sealed_wafer(vec![
        single_step_flow(),
        chain_flow(8),
        middleware_flow(8),
        parallel_flow(4),
    ]));
    let body = flow_input();

    // Sanity-check each flow once before measuring: every flow ends in the
    // echo step resolving `$.input.v` (or the previous step's `v`), so the
    // response is the original `{"v": …}` object.
    let expected: Value = serde_json::from_slice(&body).expect("input parses");
    for flow_id in [
        "bench-single-step",
        "bench-chain",
        "bench-middleware",
        "bench-parallel",
    ] {
        let out = rt.block_on(run_flow_once(&wafer, flow_id, &body));
        let got: Value = serde_json::from_slice(&out)
            .unwrap_or_else(|e| panic!("flow {flow_id} output is not JSON: {e}"));
        assert_eq!(got, expected, "flow {flow_id} output mismatch");
    }

    let mut group = c.benchmark_group("flow_dispatch");
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("single_step", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(run_flow_once(&wafer, "bench-single-step", &body).await) })
    });
    group.bench_function("chain_8_steps", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(run_flow_once(&wafer, "bench-chain", &body).await) })
    });
    group.bench_function("middleware_8_steps", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(run_flow_once(&wafer, "bench-middleware", &body).await) })
    });
    group.bench_function("parallel_4_branches", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(run_flow_once(&wafer, "bench-parallel", &body).await) })
    });

    group.finish();
}

criterion_group!(benches, bench_block_dispatch, bench_flow_dispatch);
criterion_main!(benches);
