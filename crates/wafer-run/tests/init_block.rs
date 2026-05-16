//! Wafer::init_block — the lazy init pipeline.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §3, §4

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, LifecycleType, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo, ConfigVar,
};
use wafer_run::{InitError, StaticConfigSource, Wafer};

// Test block whose lifecycle(Init) records call count.
struct FakeBlock {
    name: &'static str,
    init_calls: Arc<AtomicUsize>,
    declared: Vec<ConfigVar>,
}

#[async_trait]
impl Block for FakeBlock {
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
async fn init_block_runs_lifecycle_init_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let block = Arc::new(FakeBlock {
        name: "test/foo",
        init_calls: calls.clone(),
        declared: vec![],
    });

    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block("test/foo", block)
        .expect("register should succeed");

    wafer.init_block("test/foo").await.expect("init 1");
    wafer.init_block("test/foo").await.expect("init 2");

    assert_eq!(calls.load(Ordering::SeqCst), 1, "init must run once");
}

#[tokio::test]
async fn init_block_missing_required_config_returns_permanent() {
    let calls = Arc::new(AtomicUsize::new(0));
    let prefix = config_var_prefix("test/foo");
    let key = format!("{prefix}REQ");
    // ConfigVar::new defaults to optional=false (required), which is what we want.
    let block = Arc::new(FakeBlock {
        name: "test/foo",
        init_calls: calls.clone(),
        declared: vec![ConfigVar::new(&key, "doc", "")],
    });

    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.register_block("test/foo", block).unwrap();

    let err = wafer.init_block("test/foo").await.expect_err("must error");
    assert!(matches!(err, InitError::Permanent(_)), "got {err:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // Second call: cached error returned without re-running.
    let err2 = wafer.init_block("test/foo").await.expect_err("must error");
    assert!(matches!(err2, InitError::Permanent(_)), "got {err2:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
