//! Verify that `#[wafer_block]`-annotated native blocks are collected by
//! `linkme` and exposed via `StaticBlockRegistration`.
//!
//! Pins the link-time collection contract for the `inventory` → `linkme`
//! migration. `linkme` preserves entries even for standalone crates that have
//! no other code-level references from the consumer binary.

use std::sync::Arc;

use wafer_block::STATIC_BLOCK_REGISTRATIONS;
use wafer_block::{
    block::Block,
    context::Context,
    core_types::Message,
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
};
use wafer_block_macro::{wafer_async_trait, wafer_block};

struct TestBlock;

impl TestBlock {
    fn new() -> Self {
        Self
    }
}

#[wafer_async_trait]
impl Block for TestBlock {
    fn info(&self) -> BlockInfo {
        <TestBlock>::block_info()
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::drop_request()
    }
}

#[wafer_block(
    name = "test/linkme",
    version = "0.1.0",
    interface = "handler@v1",
    summary = "Test block for linkme registration"
)]
impl TestBlock {
    fn handle(_msg: Message, _body: Vec<u8>) -> wafer_sdk::core_abi::GuestResult {
        wafer_sdk::core_abi::GuestResult::respond(vec![])
    }
}

#[test]
fn annotated_block_appears_in_slice() {
    let names: Vec<&str> = STATIC_BLOCK_REGISTRATIONS.iter().map(|r| r.name).collect();
    assert!(
        names.contains(&"test/linkme"),
        "expected 'test/linkme' in STATIC_BLOCK_REGISTRATIONS, got: {names:?}"
    );
}

#[test]
fn factory_builds_concrete_block() {
    let reg = STATIC_BLOCK_REGISTRATIONS
        .iter()
        .find(|r| r.name == "test/linkme")
        .expect("registered");
    let block: Arc<dyn Block> = (reg.factory)();
    assert_eq!(block.info().name, "test/linkme");
}

#[test]
fn builder_loads_linkme_blocks() {
    let w = wafer_run::Wafer::builder()
        .disable_lockfile()
        .build()
        .expect("linkme-only build should succeed");
    assert!(
        w.has_block("test/linkme"),
        "WaferBuilder should register linkme-collected blocks"
    );
}

#[test]
fn block_infos_includes_linkme_registered_block() {
    let w = wafer_run::Wafer::builder()
        .disable_lockfile()
        .build()
        .expect("linkme-only build should succeed");
    let infos = w.block_infos();
    assert!(
        infos.iter().any(|i| i.name == "test/linkme"),
        "block_infos() should include linkme-registered 'test/linkme', got: {:?}",
        infos.iter().map(|i| i.name.as_str()).collect::<Vec<_>>()
    );
}
