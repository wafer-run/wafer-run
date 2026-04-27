//! Verify that `#[wafer_block]`-annotated native blocks are collected by
//! the `inventory` crate and exposed via `StaticBlockRegistration`.
//!
//! This test does NOT exercise `Wafer::builder()` — that arrives in PR γ.
//! It just pins the link-time collection contract.

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    block::Block,
    context::Context,
    core_types::Message,
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
};
use wafer_block_macro::wafer_block;
use wafer_run::{inventory, StaticBlockRegistration};

struct TestBlock;

impl TestBlock {
    fn new() -> Self {
        Self
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl Block for TestBlock {
    fn info(&self) -> BlockInfo {
        <TestBlock>::block_info()
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::drop_request()
    }
}

#[wafer_block(
    name = "test/inventory",
    version = "0.1.0",
    interface = "handler@v1",
    summary = "Test block for inventory registration"
)]
impl TestBlock {
    fn handle(_msg: Message, _body: Vec<u8>) -> wafer_sdk::core_abi::GuestResult {
        wafer_sdk::core_abi::GuestResult::respond(vec![])
    }
}

#[test]
fn annotated_block_appears_in_inventory() {
    let names: Vec<&str> = inventory::iter::<StaticBlockRegistration>()
        .map(|r| r.name)
        .collect();
    assert!(
        names.contains(&"test/inventory"),
        "expected 'test/inventory' in inventory, got: {names:?}"
    );
}

#[test]
fn factory_builds_concrete_block() {
    let reg = inventory::iter::<StaticBlockRegistration>()
        .find(|r| r.name == "test/inventory")
        .expect("registered");
    let block: Arc<dyn Block> = (reg.factory)();
    assert_eq!(block.info().name, "test/inventory");
}

#[test]
fn builder_loads_inventory_blocks() {
    let w = wafer_run::Wafer::builder()
        .disable_lockfile()
        .build()
        .expect("inventory-only build should succeed");
    assert!(
        w.has_block("test/inventory"),
        "WaferBuilder should register inventory-collected blocks"
    );
}

#[test]
fn block_infos_includes_inventory_registered_block() {
    let w = wafer_run::Wafer::builder()
        .disable_lockfile()
        .build()
        .expect("inventory-only build should succeed");
    let infos = w.block_infos();
    assert!(
        infos.iter().any(|i| i.name == "test/inventory"),
        "block_infos() should include inventory-registered 'test/inventory', got: {:?}",
        infos.iter().map(|i| i.name.as_str()).collect::<Vec<_>>()
    );
}
