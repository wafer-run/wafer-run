//! Wafer::start() must eagerly init every registered block before the
//! Start dispatch + bind() loop. Otherwise blocks like wafer-run/http-listener
//! that read config in Init and use it in bind() get an empty Init state.
//!
//! Regression: PR #98 made Init lazy and seal() skip Init; start() didn't
//! re-introduce eager Init, so blocks whose bind() depends on Init state
//! silently broke.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, LifecycleType, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo,
};
use wafer_run::{StaticConfigSource, Wafer};

struct LifecycleCountBlock {
    name: &'static str,
    init_calls: Arc<AtomicUsize>,
    start_calls: Arc<AtomicUsize>,
    bind_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Block for LifecycleCountBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "test")
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        match event.event_type {
            LifecycleType::Init => {
                self.init_calls.fetch_add(1, Ordering::SeqCst);
            }
            LifecycleType::Start => {
                // Assert Init ran before Start (Init counter must be >=1 by now).
                let init_count = self.init_calls.load(Ordering::SeqCst);
                assert!(
                    init_count >= 1,
                    "Init must run before Start; init_count = {init_count}",
                );
                self.start_calls.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
        Ok(())
    }

    fn bind(&self, _handle: Box<dyn std::any::Any + Send + Sync>) {
        // Assert Init ran before bind.
        let init_count = self.init_calls.load(Ordering::SeqCst);
        assert!(
            init_count >= 1,
            "Init must run before bind; init_count = {init_count}",
        );
        self.bind_calls.fetch_add(1, Ordering::SeqCst);
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

#[tokio::test]
async fn start_runs_init_before_start_and_bind() {
    let init_calls = Arc::new(AtomicUsize::new(0));
    let start_calls = Arc::new(AtomicUsize::new(0));
    let bind_calls = Arc::new(AtomicUsize::new(0));

    let block = Arc::new(LifecycleCountBlock {
        name: "test/eager-block",
        init_calls: init_calls.clone(),
        start_calls: start_calls.clone(),
        bind_calls: bind_calls.clone(),
    });

    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block("test/eager-block", block)
        .expect("register");

    let _arc = wafer.start().await.expect("start");

    // After start(): Init, Start, and bind each ran exactly once for our block.
    assert_eq!(
        init_calls.load(Ordering::SeqCst),
        1,
        "Init must run exactly once"
    );
    assert_eq!(
        start_calls.load(Ordering::SeqCst),
        1,
        "Start must run exactly once"
    );
    assert_eq!(
        bind_calls.load(Ordering::SeqCst),
        1,
        "bind must run exactly once"
    );
}

/// The decomposed public boot steps (`seal` → `init_all_blocks` →
/// `run_start_lifecycle` → `bind_all`) must be equivalent to `start()`.
/// Consumers that drive their own boot funnel (e.g. a native application's
/// `builder::boot`) compose these directly, so each must fire exactly once in
/// the same order `start()` guarantees (Init before Start before bind).
#[tokio::test]
async fn composed_boot_steps_match_start() {
    let init_calls = Arc::new(AtomicUsize::new(0));
    let start_calls = Arc::new(AtomicUsize::new(0));
    let bind_calls = Arc::new(AtomicUsize::new(0));

    let block = Arc::new(LifecycleCountBlock {
        name: "test/composed-block",
        init_calls: init_calls.clone(),
        start_calls: start_calls.clone(),
        bind_calls: bind_calls.clone(),
    });

    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block("test/composed-block", block)
        .expect("register");

    // Drive the boot funnel by hand, exactly as a custom native boot path does.
    wafer.seal().await.expect("seal");
    wafer.init_all_blocks().await;
    wafer.run_start_lifecycle().await;
    let _arc = wafer.bind_all();

    assert_eq!(
        init_calls.load(Ordering::SeqCst),
        1,
        "Init must run exactly once"
    );
    assert_eq!(
        start_calls.load(Ordering::SeqCst),
        1,
        "Start must run exactly once"
    );
    assert_eq!(
        bind_calls.load(Ordering::SeqCst),
        1,
        "bind must run exactly once"
    );
}
