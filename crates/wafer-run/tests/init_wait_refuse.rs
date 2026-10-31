//! `InitWait::Refuse`: dispatch never runs a block's init.
//!
//! Under `Refuse`, a dispatch outside every Init that reaches a block whose
//! init outcome is not settled answers `Unavailable` without entering the
//! block's `lifecycle(Init)` — on all three dispatch paths (`run_block`, a
//! flow step, `call_block`). `Wafer::init_block` still initializes, and a
//! `call_block` made from inside an Init still runs the callee's init
//! inline. `InitWait::Await` (the default) keeps lazy first-dispatch init.
//!
//! A transient init failure inside its backoff answers `Unavailable` with a
//! `Retry-After` response header naming the remaining backoff, in either
//! mode, and the header reaches the HTTP codec's response.

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use wafer_block::{
    context::Context,
    core_types::{ErrorCode, LifecycleEvent, LifecycleType, Message, WaferError},
    http_codec::collect_http_response,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    Block, BlockInfo,
};
use wafer_flow::{Step, WaferFlow};
use wafer_run::{runtime::slot::TRANSIENT_RETRY_BASE, InitWait, Wafer};

/// Counts Init and handle runs. Init fails with `Unavailable` (a transient
/// init failure) while `fail_init` is set, and calls `init_calls_block`
/// first when one is named.
struct Probe {
    name: &'static str,
    init_calls: AtomicUsize,
    handle_calls: AtomicUsize,
    fail_init: AtomicBool,
    init_calls_block: Option<&'static str>,
    handle_calls_block: Option<&'static str>,
}

impl Probe {
    fn with(
        name: &'static str,
        init_calls_block: Option<&'static str>,
        handle_calls_block: Option<&'static str>,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            init_calls: AtomicUsize::new(0),
            handle_calls: AtomicUsize::new(0),
            fail_init: AtomicBool::new(false),
            init_calls_block,
            handle_calls_block,
        })
    }

    fn new(name: &'static str) -> Arc<Self> {
        Self::with(name, None, None)
    }

    fn failing(name: &'static str) -> Arc<Self> {
        let probe = Self::new(name);
        probe.fail_init.store(true, Ordering::SeqCst);
        probe
    }

    fn calling_in_init(name: &'static str, callee: &'static str) -> Arc<Self> {
        Self::with(name, Some(callee), None)
    }

    fn calling_in_handle(name: &'static str, callee: &'static str) -> Arc<Self> {
        Self::with(name, None, Some(callee))
    }

    fn inits(&self) -> usize {
        self.init_calls.load(Ordering::SeqCst)
    }

    fn handles(&self) -> usize {
        self.handle_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Block for Probe {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "init-wait probe")
    }

    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        if event.event_type != LifecycleType::Init {
            return Ok(());
        }
        self.init_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(callee) = self.init_calls_block {
            let out = ctx
                .call_block(callee, Message::new(""), InputStream::empty())
                .await;
            if let Err(TerminalNotResponse::Error(e)) = out.collect_buffered().await {
                return Err(e);
            }
        }
        if self.fail_init.load(Ordering::SeqCst) {
            return Err(WaferError::new(ErrorCode::Unavailable, "backend down"));
        }
        Ok(())
    }

    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        self.handle_calls.fetch_add(1, Ordering::SeqCst);
        match self.handle_calls_block {
            Some(callee) => {
                ctx.call_block(callee, Message::new(""), InputStream::empty())
                    .await
            }
            None => OutputStream::respond(b"ok".to_vec()),
        }
    }
}

/// A sealed runtime in `mode` with `blocks` registered and the one-step flow
/// `flow-<block>` for each.
async fn runtime(mode: InitWait, blocks: &[&Arc<Probe>]) -> Wafer {
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .init_wait(mode)
        .build()
        .expect("build");
    for block in blocks {
        w.register_block(block.name, Arc::clone(block) as Arc<dyn Block>)
            .expect("register");
        w.add_flow(one_step_flow(block.name)).expect("add flow");
    }
    w.seal().await.expect("seal");
    w
}

fn one_step_flow(block: &str) -> WaferFlow {
    WaferFlow {
        id: format!("flow-{block}"),
        name: format!("one step: {block}"),
        version: "0.0.1".to_string(),
        description: None,
        input: None,
        output: None,
        steps: vec![Step {
            id: "only".to_string(),
            block: block.to_string(),
            input: None,
            next: None,
            each: None,
            parallel: None,
            description: None,
            config: None,
        }],
        config: None,
        blocks: None,
        config_map: None,
        config_defaults: None,
    }
}

async fn run_block(w: &Wafer, block: &str) -> OutputStream {
    w.run_block(block, Message::new(""), InputStream::empty())
        .await
}

async fn run_flow(w: &Wafer, block: &str) -> OutputStream {
    w.run(
        &format!("flow-{block}"),
        Message::new(""),
        InputStream::empty(),
    )
    .await
}

async fn error_of(out: OutputStream) -> WaferError {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => e,
        other => panic!("expected an Error terminal, got {other:?}"),
    }
}

async fn assert_responds(out: OutputStream) {
    match out.collect_buffered().await {
        Ok(_) => {}
        other => panic!("expected a response, got {other:?}"),
    }
}

fn retry_after(e: &WaferError) -> Option<&str> {
    e.meta
        .iter()
        .find(|m| m.key == "resp.header.Retry-After")
        .map(|m| m.value.as_str())
}

fn assert_not_initialized(e: &WaferError, block: &str) {
    assert_eq!(e.code, ErrorCode::Unavailable, "{e:?}");
    assert_eq!(
        e.message,
        format!("block `{block}` is not initialized in this runtime")
    );
    assert_eq!(
        retry_after(e),
        None,
        "an unsettled slot has no backoff to name: {e:?}"
    );
}

// --- the mode and its accessor --------------------------------------------

#[tokio::test]
async fn await_is_the_default_and_the_builder_sets_refuse() {
    let default = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .unwrap();
    assert_eq!(default.init_wait(), InitWait::Await);
    assert_eq!(InitWait::default(), InitWait::Await);

    let refuse = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .init_wait(InitWait::Refuse)
        .build()
        .unwrap();
    assert_eq!(refuse.init_wait(), InitWait::Refuse);
}

// --- never initialized → Unavailable, Init never entered --------------------

#[tokio::test]
async fn refuse_run_block_on_unsettled_slot_answers_unavailable_without_init() {
    let probe = Probe::new("test/probe");
    let w = runtime(InitWait::Refuse, &[&probe]).await;

    for _ in 0..2 {
        let e = error_of(run_block(&w, "test/probe").await).await;
        assert_not_initialized(&e, "test/probe");
    }
    assert_eq!(probe.inits(), 0, "Refuse dispatch must never enter Init");
    assert_eq!(probe.handles(), 0);
}

#[tokio::test]
async fn refuse_flow_step_on_unsettled_slot_answers_unavailable_without_init() {
    let probe = Probe::new("test/probe");
    let w = runtime(InitWait::Refuse, &[&probe]).await;

    let e = error_of(run_flow(&w, "test/probe").await).await;
    assert_not_initialized(&e, "test/probe");
    assert_eq!(probe.inits(), 0, "Refuse flow step must never enter Init");
    assert_eq!(probe.handles(), 0);
}

#[tokio::test]
async fn refuse_call_block_outside_init_on_unsettled_callee_answers_unavailable() {
    let caller = Probe::calling_in_handle("test/caller", "test/callee");
    let callee = Probe::new("test/callee");
    let w = runtime(InitWait::Refuse, &[&caller, &callee]).await;
    w.init_block("test/caller").await.expect("caller init");

    let e = error_of(run_block(&w, "test/caller").await).await;
    assert_not_initialized(&e, "test/callee");
    assert_eq!(caller.handles(), 1, "the settled caller dispatches");
    assert_eq!(callee.inits(), 0, "a handle's call_block must not run Init");
    assert_eq!(callee.handles(), 0);
}

// --- init_block settles; dispatch then serves --------------------------------

#[tokio::test]
async fn refuse_dispatches_a_block_init_block_settled() {
    let probe = Probe::new("test/probe");
    let w = runtime(InitWait::Refuse, &[&probe]).await;

    w.init_block("test/probe").await.expect("init");
    assert_eq!(probe.inits(), 1);
    assert_responds(run_block(&w, "test/probe").await).await;
    assert_responds(run_flow(&w, "test/probe").await).await;
    assert_eq!(probe.inits(), 1, "a settled slot never re-runs Init");
    assert_eq!(probe.handles(), 2);
}

#[tokio::test]
async fn refuse_keeps_failed_precondition_for_a_permanent_failure() {
    struct PermanentInit;

    #[async_trait]
    impl Block for PermanentInit {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/perm", "0.1.0", "test/iface@v1", "permanent init")
        }
        async fn lifecycle(&self, _: &dyn Context, _: LifecycleEvent) -> Result<(), WaferError> {
            Err(WaferError::new(ErrorCode::Internal, "bad config"))
        }
        async fn handle(&self, _: &dyn Context, _: Message, _: InputStream) -> OutputStream {
            OutputStream::respond(Vec::new())
        }
    }

    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .init_wait(InitWait::Refuse)
        .build()
        .unwrap();
    w.register_block("test/perm", Arc::new(PermanentInit))
        .unwrap();
    w.seal().await.unwrap();
    assert!(w.init_block("test/perm").await.is_err());

    let e = error_of(run_block(&w, "test/perm").await).await;
    assert_eq!(e.code, ErrorCode::FailedPrecondition, "{e:?}");
    assert_eq!(retry_after(&e), None);
}

// --- transient: inside the backoff, then past it -----------------------------

#[tokio::test]
async fn refuse_transient_slot_answers_unavailable_without_retry_after_then_after_backoff() {
    let probe = Probe::failing("test/probe");
    let w = runtime(InitWait::Refuse, &[&probe]).await;

    assert!(w.init_block("test/probe").await.is_err(), "Init fails");
    assert_eq!(probe.inits(), 1);

    // Right after the failure — inside the slot's backoff unless this runner
    // stalled past it, in which case the slot is already unsettled — the
    // answer is `Unavailable` with no `Retry-After` either way: under
    // `Refuse` nothing retries when the backoff ends, so the slot's backoff
    // is no delay to name.
    for out in [
        run_block(&w, "test/probe").await,
        run_flow(&w, "test/probe").await,
    ] {
        let e = error_of(out).await;
        assert_eq!(e.code, ErrorCode::Unavailable, "{e:?}");
        assert!(
            e.message.contains("init transient failure")
                || e.message.contains("is not initialized in this runtime"),
            "{e:?}"
        );
        assert_eq!(
            retry_after(&e),
            None,
            "Refuse names no slot backoff as Retry-After: {e:?}"
        );
    }

    // Past the backoff the slot is unsettled again: still no Init.
    tokio::time::sleep(TRANSIENT_RETRY_BASE * 2).await;
    for out in [
        run_block(&w, "test/probe").await,
        run_flow(&w, "test/probe").await,
    ] {
        assert_not_initialized(&error_of(out).await, "test/probe");
    }
    assert_eq!(
        probe.inits(),
        1,
        "Refuse dispatch past the backoff must not retry Init"
    );
    assert_eq!(probe.handles(), 0);

    // `init_block` is the retry: it runs Init again and settles the slot.
    probe.fail_init.store(false, Ordering::SeqCst);
    w.init_block("test/probe").await.expect("retry succeeds");
    assert_eq!(probe.inits(), 2);
    assert_responds(run_block(&w, "test/probe").await).await;
}

// --- nested Init during init_block stays inline -----------------------------

#[tokio::test]
async fn refuse_runs_a_nested_init_inline_inside_init_block() {
    let outer = Probe::calling_in_init("test/outer", "test/inner");
    let inner = Probe::new("test/inner");
    let w = runtime(InitWait::Refuse, &[&outer, &inner]).await;

    w.init_block("test/outer")
        .await
        .expect("outer Init reaches inner through call_block");
    assert_eq!(outer.inits(), 1);
    assert_eq!(inner.inits(), 1, "inner's Init ran inline, inside outer's");
    assert_eq!(
        inner.handles(),
        1,
        "outer's Init call reached inner's handle"
    );

    // Both slots are settled now, so request dispatch serves them.
    assert_responds(run_block(&w, "test/inner").await).await;
    assert_eq!(inner.inits(), 1);
}

// --- Await is unchanged -------------------------------------------------------

#[tokio::test]
async fn await_initializes_on_first_dispatch_on_every_path() {
    let caller = Probe::calling_in_handle("test/caller", "test/callee");
    let callee = Probe::new("test/callee");
    let flowed = Probe::new("test/flowed");
    let w = runtime(InitWait::Await, &[&caller, &callee, &flowed]).await;

    assert_responds(run_block(&w, "test/caller").await).await;
    assert_eq!(caller.inits(), 1, "run_block ran the caller's Init");
    assert_eq!(callee.inits(), 1, "call_block ran the callee's Init");
    assert_eq!(callee.handles(), 1);

    assert_responds(run_flow(&w, "test/flowed").await).await;
    assert_eq!(flowed.inits(), 1, "the flow step ran Init");
}

#[tokio::test]
async fn await_retries_init_in_dispatch_after_the_backoff() {
    let probe = Probe::failing("test/probe");
    let w = runtime(InitWait::Await, &[&probe]).await;

    let e = error_of(run_block(&w, "test/probe").await).await;
    assert_eq!(e.code, ErrorCode::Unavailable);
    assert_eq!(
        retry_after(&e),
        Some("1"),
        "the failing dispatch names the backoff it started: {e:?}"
    );
    assert_eq!(probe.inits(), 1);

    tokio::time::sleep(TRANSIENT_RETRY_BASE * 2).await;
    probe.fail_init.store(false, Ordering::SeqCst);
    assert_responds(run_block(&w, "test/probe").await).await;
    assert_eq!(probe.inits(), 2, "Await retries Init in the dispatch");
}

// --- Retry-After reaches the HTTP response -----------------------------------

/// Under `Await` the dispatch whose Init fails transiently names the backoff
/// it has just started, and the header reaches the HTTP response. The
/// backoff is read in that same dispatch, right after the failure, so the
/// assertion does not race the 100 ms window.
#[tokio::test]
async fn await_retry_after_reaches_the_http_codec_headers() {
    for via_flow in [false, true] {
        let probe = Probe::failing("test/probe");
        let w = runtime(InitWait::Await, &[&probe]).await;
        let out = if via_flow {
            run_flow(&w, "test/probe").await
        } else {
            run_block(&w, "test/probe").await
        };
        let parts = collect_http_response(out).await;
        assert_eq!(parts.status, 503);
        assert!(
            parts
                .headers
                .contains(&("Retry-After".to_string(), "1".to_string())),
            "via_flow={via_flow} headers: {:?}",
            parts.headers
        );
        assert_eq!(probe.inits(), 1);
    }
}

fn has_retry_after_header(headers: &[(String, String)]) -> bool {
    headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("Retry-After"))
}

/// Under `Refuse` a transient slot's 503 carries no `Retry-After`, so the
/// embedder's own delay (its rebuild window) is the one the client sees.
#[tokio::test]
async fn refuse_transient_failure_is_a_503_without_retry_after() {
    let probe = Probe::failing("test/probe");
    let w = runtime(InitWait::Refuse, &[&probe]).await;
    assert!(w.init_block("test/probe").await.is_err());

    for out in [
        run_block(&w, "test/probe").await,
        run_flow(&w, "test/probe").await,
    ] {
        let parts = collect_http_response(out).await;
        assert_eq!(parts.status, 503);
        assert!(
            !has_retry_after_header(&parts.headers),
            "headers: {:?}",
            parts.headers
        );
    }
}

#[tokio::test]
async fn an_unsettled_refusal_is_a_503_without_retry_after() {
    let probe = Probe::new("test/probe");
    let w = runtime(InitWait::Refuse, &[&probe]).await;

    let parts = collect_http_response(run_block(&w, "test/probe").await).await;
    assert_eq!(parts.status, 503);
    assert!(
        !has_retry_after_header(&parts.headers),
        "headers: {:?}",
        parts.headers
    );
}

// --- an Init in flight: refused at once, never awaited ------------------------

/// Parks its Init until released, so a dispatch can arrive while the
/// attempt holds the slot's lock.
struct GatedInit {
    inits: AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait]
impl Block for GatedInit {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/gated", "0.1.0", "test/iface@v1", "gated init")
    }

    async fn lifecycle(&self, _: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            self.inits.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }

    async fn handle(&self, _: &dyn Context, _: Message, _: InputStream) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

#[tokio::test]
async fn refuse_answers_at_once_while_an_init_is_in_flight() {
    let gated = Arc::new(GatedInit {
        inits: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .init_wait(InitWait::Refuse)
        .build()
        .unwrap();
    w.register_block("test/gated", gated.clone()).unwrap();
    w.seal().await.unwrap();

    let boot = w.init_block("test/gated");
    let request = async {
        gated.entered.notified().await;
        // The attempt holds the slot's lock now. A dispatch that waited for
        // it would never finish, since the Init is released only below.
        let answered = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_block(&w, "test/gated"),
        )
        .await;
        gated.release.notify_one();
        answered
    };
    let (booted, answered) = tokio::join!(boot, request);

    let out = answered.expect("a Refuse dispatch must not wait for an Init in flight");
    assert_not_initialized(&error_of(out).await, "test/gated");
    booted.expect("the parked Init completes once released");
    assert_eq!(
        gated.inits.load(Ordering::SeqCst),
        1,
        "the dispatch ran no Init"
    );
    assert_responds(run_block(&w, "test/gated").await).await;
}

// --- a context kept past its Init is not inside it ----------------------------

/// Keeps its Init context (`clone_arc`) for use after the Init returns, as
/// deferred work queued during an Init would.
struct KeepsInitContext {
    kept: std::sync::Mutex<Option<Arc<dyn Context>>>,
}

#[async_trait]
impl Block for KeepsInitContext {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/keeper",
            "0.1.0",
            "test/iface@v1",
            "keeps its Init ctx",
        )
    }

    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            *self.kept.lock().unwrap() = Some(ctx.clone_arc());
        }
        Ok(())
    }

    async fn handle(&self, _: &dyn Context, _: Message, _: InputStream) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

#[tokio::test]
async fn refuse_a_context_kept_past_its_init_does_not_run_init() {
    let keeper = Arc::new(KeepsInitContext {
        kept: std::sync::Mutex::new(None),
    });
    let other = Probe::new("test/other");
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .init_wait(InitWait::Refuse)
        .build()
        .unwrap();
    w.register_block("test/keeper", keeper.clone()).unwrap();
    w.register_block("test/other", Arc::clone(&other) as Arc<dyn Block>)
        .unwrap();
    w.seal().await.unwrap();

    w.init_block("test/keeper").await.expect("keeper init");
    // Taken out of the block so the handle (which reaches every block)
    // does not keep the runtime alive through a cycle.
    let kept = keeper
        .kept
        .lock()
        .unwrap()
        .take()
        .expect("Init kept its ctx");

    let out = kept
        .call_block("test/other", Message::new(""), InputStream::empty())
        .await;
    assert_not_initialized(&error_of(out).await, "test/other");
    assert_eq!(other.inits(), 0, "a kept Init context must not run Init");
    assert_eq!(other.handles(), 0);
}
