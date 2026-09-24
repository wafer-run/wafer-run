//! `lifecycle(Init)` failure handling, context and cycle refusal, driven
//! through the real dispatch paths (`Wafer::run_block`, `Wafer::init_block`,
//! flow steps and `call_block`).
//!
//! - A transient Init failure is retried after the backoff instead of being
//!   cached for the life of the process; a permanent one stays cached.
//! - Init runs on the block's own context: not the deadline, depth or
//!   identity of whoever reached the block first, and under the block's own
//!   `requires`.
//! - A panic in Init is caught and cached as a permanent failure.
//! - An init cycle is refused, including across two concurrent dispatches,
//!   and concurrent calls into one uninitialized block are not a cycle.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use wafer_block::{
    context::Context,
    core_types::{ErrorCode, LifecycleEvent, LifecycleType, Message, WaferError},
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    Block, BlockInfo,
};
use wafer_run::{runtime::slot::TRANSIENT_RETRY_BASE, ConfigSource, StaticConfigSource, Wafer};

/// What a [`Probe`] saw while its Init ran.
#[derive(Debug, Clone)]
struct Seen {
    caller_id: Option<String>,
    cancelled: bool,
    /// Outcome of the Init's `call_block`, when it made one.
    call: Option<Result<(), WaferError>>,
}

/// A block whose `lifecycle(Init)` follows a script; `handle` responds with
/// the block's name.
#[derive(Default)]
struct Probe {
    name: &'static str,
    requires: Vec<String>,
    /// Codes returned by the first Init runs, in order, before one succeeds.
    fail_first: Vec<ErrorCode>,
    /// Panic in Init.
    panic: bool,
    /// Awaited before the call, so two Inits can be held at the same point.
    barrier: Option<Arc<tokio::sync::Barrier>>,
    /// Slept before the call.
    delay: Option<Duration>,
    /// Blocks Init calls, concurrently; an error fails the Init.
    calls: Vec<&'static str>,
    init_runs: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

async fn call(ctx: &dyn Context, target: &str) -> Result<(), WaferError> {
    match ctx
        .call_block(target, Message::new(""), InputStream::empty())
        .await
        .collect_buffered()
        .await
    {
        Ok(_) => Ok(()),
        Err(TerminalNotResponse::Error(e)) => Err(e),
        Err(other) => Err(WaferError::new(ErrorCode::Internal, format!("{other:?}"))),
    }
}

#[async_trait]
impl Block for Probe {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "test").requires(self.requires.clone())
    }

    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        if event.event_type != LifecycleType::Init {
            return Ok(());
        }
        let run = self.init_runs.fetch_add(1, Ordering::SeqCst);
        assert!(!self.panic, "init exploded");
        if let Some(code) = self.fail_first.get(run) {
            return Err(WaferError::new(*code, "scripted init failure"));
        }
        if let Some(barrier) = &self.barrier {
            barrier.wait().await;
        }
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        let results = futures::future::join_all(self.calls.iter().map(|t| call(ctx, t))).await;
        let failure = results.iter().find_map(|r| r.clone().err());
        self.seen.lock().unwrap().push(Seen {
            caller_id: ctx.caller_id().map(str::to_string),
            cancelled: ctx.is_cancelled(),
            call: results.into_iter().next(),
        });
        failure.map_or(Ok(()), Err)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(self.name.as_bytes().to_vec())
    }
}

/// A block whose `handle` calls itself `msg.kind` more times (a number,
/// default 0), then calls `target` and returns its outcome — or, with
/// `fan_out`, calls `target` twice concurrently.
struct Relay {
    name: &'static str,
    target: &'static str,
    fan_out: bool,
}

#[async_trait]
impl Block for Relay {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "test")
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let remaining: u32 = msg.kind.parse().unwrap_or(0);
        let outcome = if remaining > 0 {
            ctx.call_block(
                self.name,
                Message::new((remaining - 1).to_string()),
                InputStream::empty(),
            )
            .await
            .collect_buffered()
            .await
            .map(|_| ())
            .map_err(|e| match e {
                TerminalNotResponse::Error(e) => e,
                other => WaferError::new(ErrorCode::Internal, format!("{other:?}")),
            })
        } else if self.fan_out {
            let (a, b) = futures::join!(call(ctx, self.target), call(ctx, self.target));
            a.and(b)
        } else {
            call(ctx, self.target).await
        };
        match outcome {
            Ok(()) => OutputStream::respond(b"relayed".to_vec()),
            Err(e) => OutputStream::error(e),
        }
    }
}

async fn sealed(blocks: Vec<(&'static str, Arc<dyn Block>)>) -> Arc<Wafer> {
    let cfg_src: Arc<dyn ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    for (name, block) in blocks {
        wafer.register_block(name, block).expect("register");
    }
    wafer.seal().await.expect("seal");
    Arc::new(wafer)
}

async fn run(wafer: &Wafer, block: &str) -> Result<Vec<u8>, WaferError> {
    match wafer
        .run_block(block, Message::new(""), InputStream::empty())
        .await
        .collect_buffered()
        .await
    {
        Ok(buf) => Ok(buf.body),
        Err(TerminalNotResponse::Error(e)) => Err(e),
        Err(other) => panic!("unexpected terminal: {other:?}"),
    }
}

/// Past the first retry backoff, with margin.
async fn past_first_backoff() {
    tokio::time::sleep(TRANSIENT_RETRY_BASE * 2).await;
}

#[tokio::test]
async fn transient_init_failure_is_retried_after_the_backoff() {
    let init_runs = Arc::new(AtomicUsize::new(0));
    let wafer = sealed(vec![(
        "t/flaky",
        Arc::new(Probe {
            name: "t/flaky",
            fail_first: vec![ErrorCode::Unavailable],
            init_runs: init_runs.clone(),
            ..Probe::default()
        }),
    )])
    .await;

    let first = run(&wafer, "t/flaky").await.expect_err("first init fails");
    assert_eq!(first.code, ErrorCode::Unavailable, "{first:?}");

    let inside = run(&wafer, "t/flaky")
        .await
        .expect_err("inside the backoff");
    assert_eq!(inside.code, ErrorCode::Unavailable, "{inside:?}");
    assert_eq!(
        init_runs.load(Ordering::SeqCst),
        1,
        "no retry inside the backoff"
    );

    past_first_backoff().await;
    assert_eq!(run(&wafer, "t/flaky").await.expect("retried"), b"t/flaky");
    assert_eq!(init_runs.load(Ordering::SeqCst), 2);
}

/// The embedder shape of a tolerant boot: `init_block` fails transiently,
/// boot carries on, and the block serves once a later dispatch retries.
#[tokio::test]
async fn transient_failure_during_eager_init_does_not_wedge_the_block() {
    for code in [
        ErrorCode::Unavailable,
        ErrorCode::DeadlineExceeded,
        ErrorCode::Cancelled,
        ErrorCode::ResourceExhausted,
        ErrorCode::Aborted,
    ] {
        let wafer = sealed(vec![(
            "t/flaky",
            Arc::new(Probe {
                name: "t/flaky",
                fail_first: vec![code],
                ..Probe::default()
            }),
        )])
        .await;
        let boot = wafer.init_block("t/flaky").await;
        assert!(
            matches!(boot, Err(wafer_run::runtime::slot::InitError::Transient(_))),
            "{code:?}: {boot:?}"
        );
        past_first_backoff().await;
        assert_eq!(
            run(&wafer, "t/flaky").await.expect("served after retry"),
            b"t/flaky",
            "{code:?}"
        );
    }
}

/// Guard (passes before the fix too): a permanent failure is not retried.
#[tokio::test]
async fn permanent_init_failure_stays_cached() {
    let init_runs = Arc::new(AtomicUsize::new(0));
    let wafer = sealed(vec![(
        "t/broken",
        Arc::new(Probe {
            name: "t/broken",
            fail_first: vec![ErrorCode::Internal],
            init_runs: init_runs.clone(),
            ..Probe::default()
        }),
    )])
    .await;

    let first = run(&wafer, "t/broken").await.expect_err("init fails");
    assert_eq!(first.code, ErrorCode::FailedPrecondition, "{first:?}");
    past_first_backoff().await;
    let again = run(&wafer, "t/broken").await.expect_err("still failed");
    assert_eq!(again.code, ErrorCode::FailedPrecondition, "{again:?}");
    assert_eq!(init_runs.load(Ordering::SeqCst), 1);
}

/// Init reached at the bottom of a deep `call_block` chain gets call depth 0
/// and no caller: at depth 16 its own `call_block` would otherwise be
/// refused with `ResourceExhausted`.
#[tokio::test]
async fn init_reached_deep_in_a_call_chain_runs_at_depth_zero_with_no_caller() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let wafer = sealed(vec![
        (
            "t/relay",
            Arc::new(Relay {
                name: "t/relay",
                target: "t/lazy",
                fan_out: false,
            }),
        ),
        (
            "t/lazy",
            Arc::new(Probe {
                name: "t/lazy",
                calls: vec!["t/db"],
                seen: seen.clone(),
                ..Probe::default()
            }),
        ),
        (
            "t/db",
            Arc::new(Probe {
                name: "t/db",
                ..Probe::default()
            }),
        ),
    ])
    .await;

    // 15 self-calls put the relay at depth 15, so `t/lazy` is reached at 16,
    // the maximum.
    let out = wafer
        .run_block("t/relay", Message::new("15"), InputStream::empty())
        .await
        .collect_buffered()
        .await;
    assert!(out.is_ok(), "{out:?}");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].caller_id, None, "Init has no caller");
    assert!(
        matches!(seen[0].call, Some(Ok(()))),
        "Init's own call_block runs at depth 0: {:?}",
        seen[0].call
    );
}

/// Init reached from a flow step does not inherit the flow's deadline: an
/// Init that outlasts the flow's timeout still completes its own calls.
#[tokio::test]
async fn init_reached_from_a_flow_does_not_inherit_its_deadline() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let cfg_src: Arc<dyn ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block(
            "t/relay",
            Arc::new(Relay {
                name: "t/relay",
                target: "t/slow",
                fan_out: false,
            }),
        )
        .unwrap();
    wafer
        .register_block(
            "t/slow",
            Arc::new(Probe {
                name: "t/slow",
                delay: Some(Duration::from_millis(150)),
                calls: vec!["t/db"],
                seen: seen.clone(),
                ..Probe::default()
            }),
        )
        .unwrap();
    wafer
        .register_block(
            "t/db",
            Arc::new(Probe {
                name: "t/db",
                ..Probe::default()
            }),
        )
        .unwrap();
    wafer
        .add_flow_json(
            r#"{
            "id": "short",
            "name": "short",
            "version": "0.1.0",
            "steps": [{ "id": "only", "block": "t/relay" }],
            "config": { "timeout_ms": 50 }
        }"#,
        )
        .unwrap();
    wafer.seal().await.expect("seal");

    // The flow itself may time out; what matters is `t/slow`'s Init.
    let _ = wafer
        .run("short", Message::new("0"), InputStream::empty())
        .await
        .collect_buffered()
        .await;

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert!(!seen[0].cancelled, "Init has no deadline of its own");
    assert!(
        matches!(seen[0].call, Some(Ok(()))),
        "Init's call_block is not cancelled by the flow's deadline: {:?}",
        seen[0].call
    );
    assert_eq!(run(&wafer, "t/slow").await.expect("initialized"), b"t/slow");
}

/// SEC-04 on the lazy path: Init reached through `run_block` is gated by the
/// block's `requires`, as request-time calls and eager init are.
#[tokio::test]
async fn init_reached_by_run_block_is_gated_by_requires() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let wafer = sealed(vec![
        (
            "t/guarded",
            Arc::new(Probe {
                name: "t/guarded",
                requires: vec!["t/db".to_string()],
                calls: vec!["t/secret"],
                seen: seen.clone(),
                ..Probe::default()
            }),
        ),
        (
            "t/db",
            Arc::new(Probe {
                name: "t/db",
                ..Probe::default()
            }),
        ),
        (
            "t/secret",
            Arc::new(Probe {
                name: "t/secret",
                ..Probe::default()
            }),
        ),
    ])
    .await;

    let _ = run(&wafer, "t/guarded").await;
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    match &seen[0].call {
        Some(Err(e)) => assert_eq!(e.code, ErrorCode::PermissionDenied, "{e:?}"),
        other => panic!("Init's call outside `requires` must be denied, got {other:?}"),
    }
}

#[tokio::test]
async fn init_panic_is_caught_and_cached_as_permanent() {
    let init_runs = Arc::new(AtomicUsize::new(0));
    let wafer = sealed(vec![(
        "t/panicky",
        Arc::new(Probe {
            name: "t/panicky",
            panic: true,
            init_runs: init_runs.clone(),
            ..Probe::default()
        }),
    )])
    .await;

    let first = run(&wafer, "t/panicky").await.expect_err("init panicked");
    assert_eq!(first.code, ErrorCode::FailedPrecondition, "{first:?}");
    assert!(first.message.contains("init exploded"), "{first:?}");
    let again = run(&wafer, "t/panicky").await.expect_err("cached");
    assert_eq!(again.code, ErrorCode::FailedPrecondition, "{again:?}");
    assert_eq!(
        init_runs.load(Ordering::SeqCst),
        1,
        "a panic is not retried"
    );
}

/// Two blocks whose Inits call each other, first reached by two concurrent
/// dispatches. The barrier holds both Inits (each owning its slot lock)
/// until both have started, so each dispatch must wait on the other's lock:
/// the wait that closes the cycle is refused instead of deadlocking.
#[tokio::test]
async fn concurrent_cross_init_cycle_is_refused_not_deadlocked() {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let wafer = sealed(vec![
        (
            "t/a",
            Arc::new(Probe {
                name: "t/a",
                barrier: Some(barrier.clone()),
                calls: vec!["t/b"],
                ..Probe::default()
            }),
        ),
        (
            "t/b",
            Arc::new(Probe {
                name: "t/b",
                barrier: Some(barrier),
                calls: vec!["t/a"],
                ..Probe::default()
            }),
        ),
    ])
    .await;

    let (a, b) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(run(&wafer, "t/a"), run(&wafer, "t/b"))
    })
    .await
    .expect("concurrent init cycle must not deadlock");

    for (block, out) in [("t/a", a), ("t/b", b)] {
        let e = out.expect_err("an init in a cycle fails");
        assert_eq!(e.code, ErrorCode::FailedPrecondition, "{block}: {e:?}");
        assert!(e.message.contains("init cycle detected"), "{block}: {e:?}");
    }
}

/// Concurrent calls from one frame into one uninitialized block are not a
/// cycle — the second queues on the slot lock and sees the first's init —
/// whether the frame is a top-level `handle` or runs inside an Init.
#[tokio::test]
async fn concurrent_calls_into_one_uninitialized_block_both_succeed() {
    let shared_runs = Arc::new(AtomicUsize::new(0));
    let wafer = sealed(vec![
        (
            "t/fan",
            Arc::new(Relay {
                name: "t/fan",
                target: "t/shared",
                fan_out: true,
            }),
        ),
        (
            "t/fan-init",
            Arc::new(Probe {
                name: "t/fan-init",
                calls: vec!["t/shared-2", "t/shared-2"],
                ..Probe::default()
            }),
        ),
        (
            "t/shared",
            Arc::new(Probe {
                name: "t/shared",
                delay: Some(Duration::from_millis(20)),
                init_runs: shared_runs.clone(),
                ..Probe::default()
            }),
        ),
        (
            "t/shared-2",
            Arc::new(Probe {
                name: "t/shared-2",
                delay: Some(Duration::from_millis(20)),
                ..Probe::default()
            }),
        ),
    ])
    .await;

    assert_eq!(
        run(&wafer, "t/fan").await.expect("handle fan-out"),
        b"relayed"
    );
    assert_eq!(shared_runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        run(&wafer, "t/fan-init").await.expect("Init fan-out"),
        b"t/fan-init"
    );
}
