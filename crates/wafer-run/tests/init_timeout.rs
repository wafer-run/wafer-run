//! A block's init attempt is bounded by the runtime's init timeout
//! (`WaferBuilder::init_timeout`), on every path that runs it: an Init that
//! never finishes fails its block as a transient init error naming the block
//! instead of hanging boot (`Wafer::start`), `Wafer::init_block` or a
//! dispatch to the block.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
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
use wafer_run::{InitError, InitTimeout, Wafer, DEFAULT_INIT_TIMEOUT};

/// A block whose `lifecycle(Init)` never finishes.
struct Hangs {
    init_runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Block for Hangs {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/hangs", "0.1.0", "test/iface@v1", "hangs in Init")
    }

    async fn lifecycle(&self, _ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            self.init_runs.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
        Ok(())
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(b"hangs".to_vec())
    }
}

fn wafer(timeout: InitTimeout) -> (Wafer, Arc<AtomicUsize>) {
    let init_runs = Arc::new(AtomicUsize::new(0));
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .init_timeout(timeout)
        .build()
        .expect("empty wafer build is infallible");
    w.register_block(
        "test/hangs",
        Arc::new(Hangs {
            init_runs: init_runs.clone(),
        }),
    )
    .expect("register");
    (w, init_runs)
}

fn assert_timed_out(err: &InitError, limit: Duration) {
    let InitError::Transient(msg) = err else {
        panic!("a timed-out init is transient: {err:?}");
    };
    assert!(msg.contains("test/hangs"), "names the block: {msg}");
    assert!(msg.contains("init timeout"), "{msg}");
    assert!(
        msg.contains(&format!("{limit:?}")),
        "names the limit: {msg}"
    );
}

/// Boot finishes although a block's Init never does: with the default
/// limit, the hung block fails transiently, and a request to it gets an
/// error at once rather than waiting on the Init. (Paused time: the runtime
/// advances the clock while every task waits, so the default 30 s passes at
/// once.)
#[tokio::test(start_paused = true)]
async fn a_hung_init_does_not_hang_boot() {
    let (w, init_runs) = wafer(InitTimeout::default());
    let wafer = tokio::time::timeout(2 * DEFAULT_INIT_TIMEOUT, w.start())
        .await
        .expect("boot finishes within the init timeout")
        .expect("start");
    assert_eq!(init_runs.load(Ordering::SeqCst), 1);

    match wafer
        .run_block("test/hangs", Message::new(""), InputStream::empty())
        .await
        .collect_buffered()
        .await
    {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(e.code, ErrorCode::Unavailable, "{e:?}");
            assert!(e.message.contains("test/hangs"), "{e:?}");
            assert!(e.message.contains("init timeout"), "{e:?}");
        }
        other => panic!("expected an init error, got {other:?}"),
    }
}

/// `init_block` returns the timeout as a transient error naming the block
/// and the limit, after the limit and not before.
#[tokio::test(start_paused = true)]
async fn init_block_fails_when_the_limit_passes() {
    let limit = Duration::from_millis(250);
    let (mut w, _) = wafer(InitTimeout::Limited(limit));
    w.seal().await.expect("seal");
    let started = tokio::time::Instant::now();
    let err = tokio::time::timeout(2 * limit, w.init_block("test/hangs"))
        .await
        .expect("init_block returns once the limit passes")
        .unwrap_err();
    assert_eq!(started.elapsed(), limit);
    assert_timed_out(&err, limit);
}

/// The default a builder that sets none gets.
#[test]
fn the_default_is_limited() {
    assert_eq!(
        InitTimeout::default(),
        InitTimeout::Limited(DEFAULT_INIT_TIMEOUT)
    );
}

/// `InitTimeout::Unlimited` lets Init run as long as it takes.
#[tokio::test(start_paused = true)]
async fn unlimited_waits_for_init() {
    let (mut w, _) = wafer(InitTimeout::Unlimited);
    w.seal().await.expect("seal");
    tokio::time::timeout(Duration::from_secs(3600), w.init_block("test/hangs"))
        .await
        .expect_err("an unlimited init still runs after an hour");
}

/// A block that saw `is_cancelled()` from its Init after blocking its
/// thread past the limit.
struct OverstaysSynchronously {
    saw_cancelled: Arc<AtomicBool>,
}

#[async_trait]
impl Block for OverstaysSynchronously {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/slow", "0.1.0", "test/iface@v1", "slow Init")
    }

    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            assert!(!ctx.is_cancelled(), "not cancelled when the attempt starts");
            std::thread::sleep(Duration::from_millis(60));
            self.saw_cancelled
                .store(ctx.is_cancelled(), Ordering::SeqCst);
        }
        Ok(())
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

/// The Init context carries the attempt's deadline: past the limit,
/// `is_cancelled()` is true, so an Init that checks can stop on its own.
#[tokio::test]
async fn the_init_context_reports_the_deadline() {
    let saw_cancelled = Arc::new(AtomicBool::new(false));
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .init_timeout(InitTimeout::Limited(Duration::from_millis(20)))
        .build()
        .expect("build");
    w.register_block(
        "test/slow",
        Arc::new(OverstaysSynchronously {
            saw_cancelled: saw_cancelled.clone(),
        }),
    )
    .expect("register");
    w.seal().await.expect("seal");
    let _ = w.init_block("test/slow").await;
    assert!(saw_cancelled.load(Ordering::SeqCst));
}
