//! Wafer::run_block triggers lazy init on the target block before dispatching.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §3

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, LifecycleType, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo, ConfigVar,
};
use wafer_run::{ConfigSource, StaticConfigSource, Wafer};

struct CounterBlock {
    name: &'static str,
    init_calls: Arc<AtomicUsize>,
    handle_calls: Arc<AtomicUsize>,
    declared: Vec<ConfigVar>,
}

#[async_trait]
impl Block for CounterBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "test")
            .config_keys(self.declared.clone())
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            self.init_calls.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        self.handle_calls.fetch_add(1, Ordering::SeqCst);
        OutputStream::respond(Vec::new())
    }
}

fn config_var_prefix(block: &str) -> String {
    let mut out = String::new();
    for ch in block.chars() {
        match ch {
            '/' => out.push_str("__"),
            '-' => out.push('_'),
            c => out.extend(c.to_uppercase()),
        }
    }
    out.push_str("__");
    out
}

#[tokio::test]
async fn run_block_initializes_target_on_first_call() {
    let init_calls = Arc::new(AtomicUsize::new(0));
    let handle_calls = Arc::new(AtomicUsize::new(0));

    let block = Arc::new(CounterBlock {
        name: "test/foo",
        init_calls: init_calls.clone(),
        handle_calls: handle_calls.clone(),
        declared: vec![],
    });

    let cfg_src: Arc<dyn ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.register_block("test/foo", block).expect("register");
    // run_block resolves through all_blocks; populate it without going
    // through start_without_bind() (which would itself dispatch lifecycle).
    wafer.rebuild_all_blocks();
    let wafer = Arc::new(wafer);

    // First dispatch: triggers init then handle.
    let _out1 = wafer
        .run_block("test/foo", Message::new(""), InputStream::empty())
        .await;
    assert_eq!(init_calls.load(Ordering::SeqCst), 1);
    assert_eq!(handle_calls.load(Ordering::SeqCst), 1);

    // Second dispatch: cached init; handle still runs.
    let _out2 = wafer
        .run_block("test/foo", Message::new(""), InputStream::empty())
        .await;
    assert_eq!(init_calls.load(Ordering::SeqCst), 1, "init must not re-run");
    assert_eq!(handle_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn run_block_skips_handle_if_init_fails() {
    use wafer_block::streams::output::TerminalNotResponse;

    let init_calls = Arc::new(AtomicUsize::new(0));
    let handle_calls = Arc::new(AtomicUsize::new(0));

    let key = format!("{}REQ", config_var_prefix("test/bad"));
    let block = Arc::new(CounterBlock {
        name: "test/bad",
        init_calls: init_calls.clone(),
        handle_calls: handle_calls.clone(),
        // ConfigVar::new defaults to optional=false (required).
        declared: vec![ConfigVar::new(&key, "doc", "")],
    });

    let cfg_src: Arc<dyn ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.register_block("test/bad", block).expect("register");
    wafer.rebuild_all_blocks();
    let wafer = Arc::new(wafer);

    let out = wafer
        .run_block("test/bad", Message::new(""), InputStream::empty())
        .await;
    // Config-load short-circuits because TEST_BAD__REQ is required+missing.
    // lifecycle(Init) never runs; handle must be skipped.
    assert_eq!(
        init_calls.load(Ordering::SeqCst),
        0,
        "lifecycle(Init) must not run when config load fails"
    );
    assert_eq!(
        handle_calls.load(Ordering::SeqCst),
        0,
        "handle must not run when init fails"
    );
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => {
            assert!(
                e.message.contains("init"),
                "error message should reference init: {}",
                e.message
            );
        }
        other => panic!("expected Error terminal, got {other:?}"),
    }
}
