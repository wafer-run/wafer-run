//! Executor semantics for `Step::each` (per-item fan-out) and
//! `Step::parallel` (concurrent branches), per the published
//! `waferflow/v0.1.0/flow.schema.json`:
//!
//! - `each`: "Expression resolving to an array. The step runs once per item;
//!   $.each.item and $.each.index are set." Items run sequentially in input
//!   order; the step's accumulator entry (and pipeline body) is the array of
//!   per-item outputs. A failing item fails the step fail-fast (later items
//!   are not invoked).
//! - `parallel`: "Each branch runs concurrently; results are merged into the
//!   accumulator." Branches see a snapshot of the accumulator and join before
//!   the step's own block runs. One failing branch fails the step
//!   deterministically: all branches are awaited, then the first failure in
//!   branch declaration order wins.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use serde_json::{json, Value};
use wafer_block::streams::output::TerminalNotResponse;
use wafer_flow::{NextEntry, Step, WaferFlow};
use wafer_run::*;

// ---------------------------------------------------------------------------
// Flow / step builders
// ---------------------------------------------------------------------------

fn make_flow(id: &str, steps: Vec<Step>) -> WaferFlow {
    WaferFlow {
        id: id.to_string(),
        name: format!("Test flow: {id}"),
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

fn step_with_input(id: &str, block: &str, input: Value) -> Step {
    Step {
        input: Some(input),
        ..step(id, block)
    }
}

fn branch(steps: Vec<Step>) -> wafer_flow::types::ParallelBranch {
    wafer_flow::types::ParallelBranch { steps }
}

// ---------------------------------------------------------------------------
// Test blocks
// ---------------------------------------------------------------------------

/// Echoes its input bytes back and records every input it received (as JSON).
struct RecordingEchoBlock {
    name: &'static str,
    calls: Arc<Mutex<Vec<Value>>>,
}

#[async_trait::async_trait]
impl Block for RecordingEchoBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.0.1", "http-handler@v1", "echo fixture")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        let bytes = input.collect_to_bytes().await;
        let val: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        self.calls.lock().unwrap().push(val);
        OutputStream::respond(bytes)
    }
}

/// Echoes its input unless `input.item == "boom"`, in which case it errors.
struct FailOnBoomBlock {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Block for FailOnBoomBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/fail-on-boom",
            "0.0.1",
            "http-handler@v1",
            "failing fixture",
        )
        .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let bytes = input.collect_to_bytes().await;
        let val: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if val.get("item") == Some(&json!("boom")) {
            return OutputStream::error(WaferError::new(ErrorCode::Internal, "item went boom"));
        }
        OutputStream::respond(bytes)
    }
}

/// Tracks how many invocations run concurrently. Yields several times so a
/// concurrent executor interleaves sibling invocations; a sequential executor
/// never observes more than one in flight.
struct ConcurrencyProbe {
    in_flight: AtomicUsize,
    max_seen: AtomicUsize,
}

struct ProbeEchoBlock {
    probe: Arc<ConcurrencyProbe>,
}

#[async_trait::async_trait]
impl Block for ProbeEchoBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/probe-echo",
            "0.0.1",
            "http-handler@v1",
            "concurrency probe fixture",
        )
        .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        let bytes = input.collect_to_bytes().await;
        let now = self.probe.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.probe.max_seen.fetch_max(now, Ordering::SeqCst);
        for _ in 0..8 {
            tokio::task::yield_now().await;
            let now = self.probe.in_flight.load(Ordering::SeqCst);
            self.probe.max_seen.fetch_max(now, Ordering::SeqCst);
        }
        self.probe.in_flight.fetch_sub(1, Ordering::SeqCst);
        OutputStream::respond(bytes)
    }
}

/// Errors with a fixed message after yielding `yields` times — used to make
/// one branch fail later (in wall-clock terms) than another.
struct DelayedFailBlock {
    name: &'static str,
    yields: usize,
    message: &'static str,
}

#[async_trait::async_trait]
impl Block for DelayedFailBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            self.name,
            "0.0.1",
            "http-handler@v1",
            "delayed-fail fixture",
        )
        .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        for _ in 0..self.yields {
            tokio::task::yield_now().await;
        }
        OutputStream::error(WaferError::new(ErrorCode::Internal, self.message))
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

async fn run_flow(w: &Wafer, flow_id: &str, input: Value) -> Result<Value, WaferError> {
    let bytes = serde_json::to_vec(&input).expect("test input serializes");
    let output = w
        .run(
            flow_id,
            Message::new("http.request"),
            InputStream::from_bytes(bytes),
        )
        .await;
    match output.collect_buffered().await {
        Ok(resp) => Ok(serde_json::from_slice(&resp.body).unwrap_or(Value::Null)),
        Err(TerminalNotResponse::Error(e)) => Err(e),
        other => panic!("expected Response or Error terminal, got {other:?}"),
    }
}

async fn sealed_wafer(blocks: Vec<(&str, Arc<dyn Block>)>, flow: WaferFlow) -> Wafer {
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    for (name, block) in blocks {
        w.register_block(name, block).unwrap();
    }
    w.add_flow(flow);
    w.seal().await.expect("seal failed");
    w
}

// ---------------------------------------------------------------------------
// each
// ---------------------------------------------------------------------------

#[tokio::test]
async fn each_collects_results_in_input_order() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let echo = RecordingEchoBlock {
        name: "test/echo-each",
        calls: calls.clone(),
    };

    let mut fanout = step_with_input(
        "fanout",
        "test/echo-each",
        json!({ "item": "$.each.item", "index": "$.each.index" }),
    );
    fanout.each = Some("$.input.items".to_string());

    let w = sealed_wafer(
        vec![("test/echo-each", Arc::new(echo))],
        make_flow("each-order", vec![fanout]),
    )
    .await;

    let out = run_flow(&w, "each-order", json!({ "items": ["a", "b", "c"] }))
        .await
        .expect("flow should succeed");

    // Results collected in input order, with $.each.item / $.each.index
    // visible to the inner step's input template.
    let expected = json!([
        { "item": "a", "index": 0 },
        { "item": "b", "index": 1 },
        { "item": "c", "index": 2 },
    ]);
    assert_eq!(
        out, expected,
        "step output must be the ordered results array"
    );
    assert_eq!(
        *calls.lock().unwrap(),
        expected.as_array().unwrap().clone(),
        "block must be invoked once per item, in input order"
    );
}

#[tokio::test]
async fn each_over_empty_list_completes_without_block_calls() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let echo = RecordingEchoBlock {
        name: "test/echo-empty",
        calls: calls.clone(),
    };

    let mut fanout = step_with_input(
        "fanout",
        "test/echo-empty",
        json!({ "item": "$.each.item" }),
    );
    fanout.each = Some("$.input.items".to_string());

    let w = sealed_wafer(
        vec![("test/echo-empty", Arc::new(echo))],
        make_flow("each-empty", vec![fanout]),
    )
    .await;

    let out = run_flow(&w, "each-empty", json!({ "items": [] }))
        .await
        .expect("empty fan-out should succeed");

    assert_eq!(
        out,
        json!([]),
        "empty list must produce an empty results array"
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "block must not be invoked for an empty list"
    );
}

#[tokio::test]
async fn each_resolves_nested_data_paths() {
    let prep_calls = Arc::new(Mutex::new(Vec::new()));
    let each_calls = Arc::new(Mutex::new(Vec::new()));

    let prep = RecordingEchoBlock {
        name: "test/echo-prep",
        calls: prep_calls,
    };
    let echo = RecordingEchoBlock {
        name: "test/echo-nested",
        calls: each_calls.clone(),
    };

    let prep_step = step_with_input(
        "prep",
        "test/echo-prep",
        json!({ "data": { "items": ["x", "y"] } }),
    );
    let mut fanout = step_with_input("fanout", "test/echo-nested", json!("$.each.item"));
    fanout.each = Some("$.prep.data.items".to_string());

    let w = sealed_wafer(
        vec![
            ("test/echo-prep", Arc::new(prep)),
            ("test/echo-nested", Arc::new(echo)),
        ],
        make_flow("each-nested", vec![prep_step, fanout]),
    )
    .await;

    let out = run_flow(&w, "each-nested", json!({}))
        .await
        .expect("nested each should succeed");

    assert_eq!(out, json!(["x", "y"]));
    assert_eq!(*each_calls.lock().unwrap(), vec![json!("x"), json!("y")]);
}

#[tokio::test]
async fn each_without_input_template_passes_item_as_input() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let echo = RecordingEchoBlock {
        name: "test/echo-raw-item",
        calls: calls.clone(),
    };

    let mut fanout = step("fanout", "test/echo-raw-item");
    fanout.each = Some("$.input.items".to_string());

    let w = sealed_wafer(
        vec![("test/echo-raw-item", Arc::new(echo))],
        make_flow("each-raw", vec![fanout]),
    )
    .await;

    let out = run_flow(&w, "each-raw", json!({ "items": [{ "n": 1 }, { "n": 2 }] }))
        .await
        .expect("raw-item each should succeed");

    assert_eq!(out, json!([{ "n": 1 }, { "n": 2 }]));
    assert_eq!(
        *calls.lock().unwrap(),
        vec![json!({ "n": 1 }), json!({ "n": 2 })],
        "without an input template, each item itself is the block input"
    );
}

#[tokio::test]
async fn each_item_failure_fails_step_fail_fast() {
    let calls = Arc::new(AtomicUsize::new(0));
    let block = FailOnBoomBlock {
        calls: calls.clone(),
    };

    let mut fanout = step_with_input(
        "fanout",
        "test/fail-on-boom",
        json!({ "item": "$.each.item" }),
    );
    fanout.each = Some("$.input.items".to_string());

    let w = sealed_wafer(
        vec![("test/fail-on-boom", Arc::new(block))],
        make_flow("each-fail", vec![fanout]),
    )
    .await;

    let err = run_flow(&w, "each-fail", json!({ "items": ["ok", "boom", "never"] }))
        .await
        .expect_err("a failing item must fail the step");

    assert!(
        err.message.contains("boom"),
        "error must carry the failing item's error, got: {}",
        err.message
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "fail-fast: items after the failing one must not be invoked"
    );
}

#[tokio::test]
async fn each_over_non_array_value_errors() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let echo = RecordingEchoBlock {
        name: "test/echo-non-array",
        calls: calls.clone(),
    };

    let mut fanout = step_with_input(
        "fanout",
        "test/echo-non-array",
        json!({ "item": "$.each.item" }),
    );
    fanout.each = Some("$.input.items".to_string());

    let w = sealed_wafer(
        vec![("test/echo-non-array", Arc::new(echo))],
        make_flow("each-non-array", vec![fanout]),
    )
    .await;

    let err = run_flow(&w, "each-non-array", json!({ "items": "not-an-array" }))
        .await
        .expect_err("each over a non-array must error");

    assert!(
        err.message.contains("array"),
        "error must say the each-path did not resolve to an array, got: {}",
        err.message
    );
    assert!(calls.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// parallel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn parallel_branches_run_concurrently_and_merge_into_accumulator() {
    let probe = Arc::new(ConcurrencyProbe {
        in_flight: AtomicUsize::new(0),
        max_seen: AtomicUsize::new(0),
    });
    let probe_block = ProbeEchoBlock {
        probe: probe.clone(),
    };
    let join_calls = Arc::new(Mutex::new(Vec::new()));
    let join_block = RecordingEchoBlock {
        name: "test/echo-join",
        calls: join_calls,
    };

    // Outer step: two branches fan out, then the step's own block runs as the
    // join and can reference both branches' outputs.
    let mut outer = step_with_input(
        "join",
        "test/echo-join",
        json!({ "a": "$.side-a.who", "b": "$.side-b.who" }),
    );
    outer.parallel = Some(vec![
        branch(vec![step_with_input(
            "side-a",
            "test/probe-echo",
            json!({ "who": "a" }),
        )]),
        branch(vec![step_with_input(
            "side-b",
            "test/probe-echo",
            json!({ "who": "b" }),
        )]),
    ]);

    let w = sealed_wafer(
        vec![
            ("test/probe-echo", Arc::new(probe_block)),
            ("test/echo-join", Arc::new(join_block)),
        ],
        make_flow("parallel-join", vec![outer]),
    )
    .await;

    let out = run_flow(&w, "parallel-join", json!({}))
        .await
        .expect("parallel flow should succeed");

    assert_eq!(
        out,
        json!({ "a": "a", "b": "b" }),
        "branch outputs must be merged into the accumulator before the join block runs"
    );
    assert_eq!(
        probe.max_seen.load(Ordering::SeqCst),
        2,
        "both branches must be in flight at once (concurrent execution)"
    );
}

#[tokio::test]
async fn parallel_branch_error_first_in_declaration_order_wins() {
    // Branch 0 fails LAST in wall-clock terms (it yields first), branch 1
    // fails immediately. The reported error must still be branch 0's:
    // all branches are awaited, then the first failure in branch declaration
    // order wins — completion timing must not influence the outcome.
    let slow = DelayedFailBlock {
        name: "test/fail-slow",
        yields: 8,
        message: "error-zero",
    };
    let fast = DelayedFailBlock {
        name: "test/fail-fast",
        yields: 0,
        message: "error-one",
    };
    let join_block = RecordingEchoBlock {
        name: "test/echo-noreach",
        calls: Arc::new(Mutex::new(Vec::new())),
    };

    let mut outer = step_with_input("join", "test/echo-noreach", json!({}));
    outer.parallel = Some(vec![
        branch(vec![step_with_input("b0", "test/fail-slow", json!({}))]),
        branch(vec![step_with_input("b1", "test/fail-fast", json!({}))]),
    ]);

    let w = sealed_wafer(
        vec![
            ("test/fail-slow", Arc::new(slow)),
            ("test/fail-fast", Arc::new(fast)),
            ("test/echo-noreach", Arc::new(join_block)),
        ],
        make_flow("parallel-err", vec![outer]),
    )
    .await;

    let err = run_flow(&w, "parallel-err", json!({}))
        .await
        .expect_err("a failing branch must fail the step");

    assert!(
        err.message.contains("error-zero"),
        "first failure in branch declaration order must win, got: {}",
        err.message
    );
}

#[tokio::test]
async fn parallel_branch_step_with_next_routing_is_rejected() {
    let echo = RecordingEchoBlock {
        name: "test/echo-next",
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let join_block = RecordingEchoBlock {
        name: "test/echo-join2",
        calls: Arc::new(Mutex::new(Vec::new())),
    };

    let mut inner = step_with_input("inner", "test/echo-next", json!({}));
    inner.next = Some(vec![NextEntry {
        when: None,
        step: Some("join".to_string()),
        flow: None,
    }]);

    let mut outer = step_with_input("join", "test/echo-join2", json!({}));
    outer.parallel = Some(vec![branch(vec![inner])]);

    let w = sealed_wafer(
        vec![
            ("test/echo-next", Arc::new(echo)),
            ("test/echo-join2", Arc::new(join_block)),
        ],
        make_flow("parallel-next", vec![outer]),
    )
    .await;

    let err = run_flow(&w, "parallel-next", json!({}))
        .await
        .expect_err("next routing inside a parallel branch must be rejected");

    assert!(
        err.message.contains("next"),
        "error must name the unsupported 'next' routing, got: {}",
        err.message
    );
}
