//! A block's init attempt is bounded by its init budget — what the block
//! declares (`BlockInfo::init_timeout`), capped by the embedder's
//! `WaferBuilder::init_timeout` — on every path that runs it. An attempt over
//! its budget fails as a transient init error naming the block and the
//! budget, instead of hanging boot (`Wafer::start`), `Wafer::init_block` or a
//! dispatch; and an Init that fails once its deadline has passed is that same
//! transient timeout, never a permanent failure cached for the runtime's
//! life. With no budget at all, Init runs as long as it takes.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
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
use wafer_run::{InitError, Wafer};

/// A block whose `lifecycle(Init)` never finishes, declaring `budget`.
struct Hangs {
    budget: Option<Duration>,
}

#[async_trait]
impl Block for Hangs {
    fn info(&self) -> BlockInfo {
        let info = BlockInfo::new("test/hangs", "0.1.0", "test/iface@v1", "hangs in Init");
        match self.budget {
            Some(budget) => info.init_timeout(budget),
            None => info,
        }
    }

    async fn lifecycle(&self, _ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            std::future::pending::<()>().await;
        }
        Ok(())
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(b"hangs".to_vec())
    }
}

fn wafer(cap: Option<Duration>, blocks: Vec<(&str, Arc<dyn Block>)>) -> Wafer {
    let mut builder = Wafer::builder().disable_inventory().disable_lockfile();
    if let Some(cap) = cap {
        builder = builder.init_timeout(cap);
    }
    let mut w = builder.build().expect("empty wafer build is infallible");
    for (name, block) in blocks {
        w.register_block(name, block).expect("register");
    }
    w
}

fn hangs(budget: Option<Duration>) -> Vec<(&'static str, Arc<dyn Block>)> {
    vec![("test/hangs", Arc::new(Hangs { budget }))]
}

fn transient(err: &InitError) -> &str {
    match err {
        InitError::Transient(msg) => msg,
        other => panic!("an init over its budget is transient: {other:?}"),
    }
}

/// Boot finishes although a block's Init never does: the hung block fails
/// transiently once its declared budget passes, and a request to it gets an
/// error at once rather than waiting on the Init.
#[tokio::test]
async fn a_hung_init_does_not_hang_boot() {
    let w = wafer(None, hangs(Some(Duration::from_millis(50))));
    let wafer = tokio::time::timeout(Duration::from_secs(10), w.start())
        .await
        .expect("boot finishes once the budget passes")
        .expect("start");

    match wafer
        .run_block("test/hangs", Message::new(""), InputStream::empty())
        .await
        .collect_buffered()
        .await
    {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(e.code, ErrorCode::Unavailable, "{e:?}");
            assert!(e.message.contains("test/hangs"), "{e:?}");
            assert!(e.message.contains("init budget of 50ms"), "{e:?}");
        }
        other => panic!("expected an init error, got {other:?}"),
    }
}

/// `init_block` fails transiently after the declared budget, naming the
/// block, the budget, who set it, and that the abandoned work may still run.
#[tokio::test]
async fn init_block_fails_when_the_declared_budget_passes() {
    let budget = Duration::from_millis(100);
    let mut w = wafer(None, hangs(Some(budget)));
    w.seal().await.expect("seal");
    let started = std::time::Instant::now();
    let err = tokio::time::timeout(Duration::from_secs(10), w.init_block("test/hangs"))
        .await
        .expect("init_block returns once the budget passes")
        .unwrap_err();
    assert!(started.elapsed() >= budget, "{:?}", started.elapsed());
    let msg = transient(&err);
    assert!(msg.contains("block `test/hangs`"), "{msg}");
    assert!(msg.contains("init budget of 100ms"), "{msg}");
    assert!(msg.contains("declared by the block"), "{msg}");
    assert!(msg.contains("may still be running"), "{msg}");
}

/// The embedder's cap bounds a block that declares no budget, and wins
/// over a larger declared one.
#[tokio::test]
async fn the_embedders_cap_bounds_every_block() {
    for declared in [None, Some(Duration::from_secs(3600))] {
        let mut w = wafer(Some(Duration::from_millis(50)), hangs(declared));
        w.seal().await.expect("seal");
        let err = tokio::time::timeout(Duration::from_secs(10), w.init_block("test/hangs"))
            .await
            .expect("init_block returns once the cap passes")
            .unwrap_err();
        let msg = transient(&err);
        assert!(msg.contains("init budget of 50ms"), "{msg}");
        assert!(msg.contains("the runtime's init_timeout cap"), "{msg}");
    }
}

/// With no declared budget and no cap, Init runs as long as it takes.
#[tokio::test]
async fn without_a_budget_init_runs_as_long_as_it_takes() {
    let mut w = wafer(None, hangs(None));
    w.seal().await.expect("seal");
    tokio::time::timeout(Duration::from_millis(300), w.init_block("test/hangs"))
        .await
        .expect_err("an Init without a budget is still running");
}

/// A block that saw `is_cancelled()` from its Init after blocking its
/// thread past its budget.
struct OverstaysSynchronously {
    saw_cancelled: Arc<AtomicBool>,
}

#[async_trait]
impl Block for OverstaysSynchronously {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/slow", "0.1.0", "test/iface@v1", "slow Init")
            .init_timeout(Duration::from_millis(20))
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

/// The Init context carries the attempt's deadline: past the budget,
/// `is_cancelled()` is true, so an Init that checks can stop on its own.
#[tokio::test]
async fn the_init_context_reports_the_deadline() {
    let saw_cancelled = Arc::new(AtomicBool::new(false));
    let mut w = wafer(
        None,
        vec![(
            "test/slow",
            Arc::new(OverstaysSynchronously {
                saw_cancelled: saw_cancelled.clone(),
            }),
        )],
    );
    w.seal().await.expect("seal");
    let _ = w.init_block("test/slow").await;
    assert!(saw_cancelled.load(Ordering::SeqCst));
}

/// A database-like block: each call takes a fraction of a millisecond.
struct Db;

#[async_trait]
impl Block for Db {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/db", "0.1.0", "test/db@v1", "fast statements")
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        std::thread::sleep(Duration::from_micros(300));
        OutputStream::respond(Vec::new())
    }
}

/// A migration runner shaped like an application's: its Init runs
/// statements through `call_block` until one fails, and reports any failure
/// as `Internal` — which alone would classify as a permanent init failure.
struct Migrates {
    codes_seen: Arc<Mutex<Vec<ErrorCode>>>,
}

#[async_trait]
impl Block for Migrates {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/migrates", "0.1.0", "test/iface@v1", "long migration")
            .requires(vec!["test/db".to_string()])
            .init_timeout(Duration::from_millis(40))
    }

    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        if event.event_type != LifecycleType::Init {
            return Ok(());
        }
        loop {
            let out = ctx
                .call_block("test/db", Message::new("stmt"), InputStream::empty())
                .await
                .collect_buffered()
                .await;
            if let Err(TerminalNotResponse::Error(e)) = out {
                self.codes_seen.lock().unwrap().push(e.code);
                return Err(WaferError::new(
                    ErrorCode::Internal,
                    format!("migration failed: {e}"),
                ));
            }
        }
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

/// An Init that runs out of budget mid-migration and reports the refused
/// statement as `Internal` still fails transiently — retried later — every
/// time, never as a permanent failure. The refused `call_block` answered
/// `DeadlineExceeded`.
#[tokio::test]
async fn an_init_failing_past_its_deadline_is_the_timeout_not_permanent() {
    for trial in 0..30 {
        let codes_seen = Arc::new(Mutex::new(Vec::new()));
        let mut w = wafer(
            None,
            vec![
                ("test/db", Arc::new(Db)),
                (
                    "test/migrates",
                    Arc::new(Migrates {
                        codes_seen: codes_seen.clone(),
                    }),
                ),
            ],
        );
        w.seal().await.expect("seal");
        let err = tokio::time::timeout(Duration::from_secs(10), w.init_block("test/migrates"))
            .await
            .expect("init_block returns once the budget passes")
            .unwrap_err();
        let msg = transient(&err);
        assert!(msg.contains("init budget of 40ms"), "trial {trial}: {msg}");
        for code in codes_seen.lock().unwrap().iter() {
            assert_eq!(*code, ErrorCode::DeadlineExceeded, "trial {trial}");
        }
    }
}
