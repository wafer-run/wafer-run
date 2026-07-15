//! e2e tests for opt-in warm instance pooling of wasmi guests (PERF-01
//! Part B).
//!
//! The fixture (`tests/pool_guest/`, built by `scripts/build-fixtures.sh` in
//! two variants) keeps a global call counter in linear memory — its value is
//! the direct observable for "was this instance reused?":
//!
//!   - `pool_guest_singleton.wasm` declares `InstanceMode::Singleton` →
//!     pool-eligible; sequential calls count 1, 2, …
//!   - `pool_guest_percall.wasm` declares `InstanceMode::PerExecution` →
//!     stays cold; every call counts 1.
//!
//! The host kill switch (`WAFER_RUN_WASM_POOLING`) is covered separately in
//! `tests/wasm_pooling_env_kill_switch.rs` — its own integration-test binary,
//! because it mutates process-global env that every `WasmiBlock` load reads.

#![cfg(feature = "wasm")]

use std::{path::PathBuf, sync::Arc};

use wafer_block::streams::output::TerminalNotResponse;
use wafer_run::{
    wasm::WasmiBlock, Block, BlockInfo, Context, ErrorCode, InputStream, Message, OutputStream,
    ResourceLimits, Wafer, WaferError,
};

// ---------------------------------------------------------------------------
// Fixture discovery
// ---------------------------------------------------------------------------

/// Read a prebuilt pool-guest variant (`"singleton"` or `"percall"`).
fn pool_guest_wasm(variant: &str) -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(format!(
        "tests/pool_guest/target/wasm32-wasip1/release/pool_guest_{variant}.wasm"
    ));
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "failed to read pool guest wasm at {}: {e}\n\
             Did you build the fixtures first?\n  bash scripts/build-fixtures.sh",
            p.display()
        )
    })
}

// ---------------------------------------------------------------------------
// Mock context — for direct `WasmiBlock::handle` tests (no nested dispatch).
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MockContext;

#[async_trait::async_trait]
impl Context for MockContext {
    async fn call_block(&self, _name: &str, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            "mock context: call_block not supported",
        ))
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

/// Drive one `handle` call and return the response body as UTF-8, or the
/// terminal error.
async fn call(block: &WasmiBlock, kind: &str) -> Result<String, WaferError> {
    let out = block
        .handle(&MockContext, Message::new(kind), InputStream::empty())
        .await;
    match out.collect_buffered().await {
        Ok(buf) => Ok(String::from_utf8(buf.body).expect("pool-guest responds UTF-8")),
        Err(TerminalNotResponse::Error(e)) => Err(e),
        Err(other) => panic!("unexpected non-error terminal: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Sequential reuse + cold control
// ---------------------------------------------------------------------------

/// A Singleton-declared guest keeps its instance across sequential calls:
/// the global counter counts up, and exactly one instance idles in the pool
/// between calls.
#[tokio::test]
async fn singleton_guest_reuses_instance_across_sequential_calls() {
    let block = WasmiBlock::load_from_bytes(&pool_guest_wasm("singleton")).expect("load");

    assert_eq!(call(&block, "pool.count").await.unwrap(), "1");
    assert_eq!(
        block.pooled_instance_count(),
        1,
        "clean exit must check the instance back in"
    );
    assert_eq!(
        call(&block, "pool.count").await.unwrap(),
        "2",
        "second call must reuse the pooled instance (counter survives)"
    );
    assert_eq!(call(&block, "pool.count").await.unwrap(), "3");
    assert_eq!(block.pooled_instance_count(), 1);
}

/// The identical guest declaring `PerExecution` must stay cold: every call
/// sees a fresh instance (counter always 1) and nothing is ever pooled.
#[tokio::test]
async fn percall_guest_gets_a_fresh_instance_every_call() {
    let block = WasmiBlock::load_from_bytes(&pool_guest_wasm("percall")).expect("load");

    assert_eq!(call(&block, "pool.count").await.unwrap(), "1");
    assert_eq!(
        call(&block, "pool.count").await.unwrap(),
        "1",
        "PerExecution must not reuse instances"
    );
    assert_eq!(
        block.pooled_instance_count(),
        0,
        "a non-opted block must never pool instances"
    );
}

// ---------------------------------------------------------------------------
// Failure replacement
// ---------------------------------------------------------------------------

/// A trap drops the pooled instance; the next call starts fresh (counter
/// back to 1) instead of resuming a possibly-corrupted instance.
#[tokio::test]
async fn trap_drops_the_instance_and_next_call_starts_fresh() {
    let block = WasmiBlock::load_from_bytes(&pool_guest_wasm("singleton")).expect("load");

    assert_eq!(call(&block, "pool.count").await.unwrap(), "1");
    assert_eq!(block.pooled_instance_count(), 1);

    let err = call(&block, "pool.trap")
        .await
        .expect_err("trap arm must surface as an error terminal");
    assert_eq!(err.code, ErrorCode::Internal, "trap error: {err}");
    assert_eq!(
        block.pooled_instance_count(),
        0,
        "trapped instance must be dropped, not checked back in"
    );

    assert_eq!(
        call(&block, "pool.count").await.unwrap(),
        "1",
        "call after a trap must run on a fresh instance"
    );
}

// ---------------------------------------------------------------------------
// Per-call state reset: leaked stream handles
// ---------------------------------------------------------------------------

/// A guest that opens a stream handle and never closes it must not see that
/// handle survive into its next (reused) invocation: checkin drains the
/// `StreamRegistry`, so handle numbering restarts at 1 on a fresh registry.
/// Response format is `"{counter}:{handle}"` — the counter proves the
/// instance itself WAS reused, so a fresh registry can't be explained away
/// by a fresh instance.
#[tokio::test]
async fn leaked_stream_handles_are_drained_on_checkin() {
    let block = WasmiBlock::load_from_bytes(&pool_guest_wasm("singleton")).expect("load");

    assert_eq!(call(&block, "pool.leak_stream").await.unwrap(), "1:1");
    assert_eq!(
        call(&block, "pool.leak_stream").await.unwrap(),
        "2:1",
        "reused instance (counter 2) must start with an empty stream \
         registry (handle numbering back at 1)"
    );
}

// ---------------------------------------------------------------------------
// Recycle on memory growth
// ---------------------------------------------------------------------------

/// An instance whose linear memory grew to the block's page cap is recycled
/// on checkin instead of being reused with a saturated heap. Sequence:
/// count → 1 (fresh), grow_to_cap → 2 (reused — proves pooling was active),
/// count → 1 (the saturated instance was dropped; fresh again).
#[tokio::test]
async fn memory_grown_to_cap_recycles_the_instance() {
    // Small memory cap so the grow loop is quick and cheap on fuel.
    let block = WasmiBlock::load_from_bytes_with_limits(
        &pool_guest_wasm("singleton"),
        ResourceLimits {
            memory_pages: 64, // 4 MiB
            ..ResourceLimits::default()
        },
    )
    .expect("load");

    assert_eq!(call(&block, "pool.count").await.unwrap(), "1");
    assert_eq!(
        call(&block, "pool.grow_to_cap").await.unwrap(),
        "2",
        "grow arm must run on the reused instance"
    );
    assert_eq!(
        block.pooled_instance_count(),
        0,
        "instance at the memory cap must be recycled, not pooled"
    );
    assert_eq!(
        call(&block, "pool.count").await.unwrap(),
        "1",
        "call after recycle must run on a fresh instance"
    );
}

// ---------------------------------------------------------------------------
// Concurrency + pool cap (through the full runtime, gated on a barrier)
// ---------------------------------------------------------------------------

/// Native gate block: parks every call on a shared barrier, then echoes.
/// Guests that nested-call it are provably mid-call simultaneously once the
/// test task joins the barrier.
struct GateBlock {
    barrier: Arc<tokio::sync::Barrier>,
}

#[async_trait::async_trait]
impl Block for GateBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/pool-gate",
            "0.0.0",
            "handler@v1",
            "Barrier gate for instance-pooling concurrency tests",
        )
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        self.barrier.wait().await;
        OutputStream::respond(body)
    }
}

/// Build a started runtime with the gate block (parked on `barrier`) and the
/// Singleton pool guest; returns the runtime plus a handle to the guest for
/// pool inspection.
async fn gated_runtime(barrier: Arc<tokio::sync::Barrier>) -> (Arc<Wafer>, Arc<WasmiBlock>) {
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    w.register_block("test/pool-gate", Arc::new(GateBlock { barrier }))
        .expect("register gate");
    let guest =
        Arc::new(WasmiBlock::load_from_bytes(&pool_guest_wasm("singleton")).expect("load guest"));
    w.register_block("test/pool-guest", guest.clone())
        .expect("register pool guest");
    let wafer = w.start().await.expect("start runtime");
    (wafer, guest)
}

/// Run one call of `kind` through the runtime and return the counter the
/// guest reported.
async fn runtime_call(wafer: &Wafer, kind: &str) -> String {
    let out = wafer
        .run_block("test/pool-guest", Message::new(kind), InputStream::empty())
        .await;
    let buf = out
        .collect_buffered()
        .await
        .unwrap_or_else(|t| panic!("{kind} should Respond, got: {t:?}"));
    String::from_utf8(buf.body).expect("utf8 counter")
}

/// Run one `pool.gate` call through the runtime — parks inside the gate
/// until its barrier fires.
async fn gate_call(wafer: &Wafer) -> String {
    runtime_call(wafer, "pool.gate").await
}

/// Overlapping calls must each get their own instance — checkout is
/// exclusive ownership, never sharing a Store between concurrent callers.
/// Both overlapped calls report counter 1 (two distinct fresh instances);
/// a follow-up sequential call reuses one of them and reports 2.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_calls_get_distinct_instances() {
    let barrier = Arc::new(tokio::sync::Barrier::new(3)); // 2 guests + test
    let (wafer, guest) = gated_runtime(barrier.clone()).await;

    let (a, b, _) = tokio::join!(gate_call(&wafer), gate_call(&wafer), barrier.wait());
    assert_eq!(
        (a.as_str(), b.as_str()),
        ("1", "1"),
        "overlapping calls must run on distinct fresh instances \
         (a shared instance would have reported a counter of 2)"
    );
    assert_eq!(
        guest.pooled_instance_count(),
        2,
        "both instances check back in after clean exits"
    );

    // Sequential follow-up pops a warm instance: counter continues at 2.
    // (`pool.count`, NOT `pool.gate` — the 3-slot barrier cycle is spent,
    // and a lone gate call would park forever waiting for two more.)
    assert_eq!(runtime_call(&wafer, "pool.count").await, "2");
}

/// The pool retains at most 4 idle instances; surplus clean-exiting
/// instances are dropped on checkin, not queued.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn pool_cap_bounds_retained_instances() {
    const CONCURRENT: usize = 6; // > MAX_POOLED_INSTANCES (4)
    let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENT + 1));
    let (wafer, guest) = gated_runtime(barrier.clone()).await;

    let calls = (0..CONCURRENT).map(|_| gate_call(&wafer));
    let (counters, _) = tokio::join!(futures::future::join_all(calls), barrier.wait());
    assert_eq!(
        counters,
        vec!["1"; CONCURRENT],
        "all overlapped calls must run on distinct fresh instances"
    );
    assert_eq!(
        guest.pooled_instance_count(),
        4,
        "checkin must retain at most the pool cap (4) and drop the surplus"
    );
}
