//! Test that Wafer exposes registered block names via `block_names()`.

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::BlockInfo;
use wafer_run::{context::Context, Block, InputStream, Message, OutputStream, Wafer};

struct StubBlock {
    name: &'static str,
}

#[async_trait]
impl Block for StubBlock {
    fn info(&self) -> BlockInfo {
        // Deliberately self-reports a DIFFERENT name than the registration
        // key — block_names() must return registration keys, not info names.
        BlockInfo::new(self.name, "0.0.1", "http-handler@v1", "stub")
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(Vec::<u8>::new())
    }
}

#[test]
fn block_names_returns_sorted_registration_keys() {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    wafer
        .register_block("test/zeta", Arc::new(StubBlock { name: "test/zeta" }))
        .expect("register zeta");
    wafer
        .register_block(
            "test/alpha",
            Arc::new(StubBlock {
                name: "test/self-reported-other",
            }),
        )
        .expect("register alpha");

    // Sorted registration keys — NOT info().name.
    assert_eq!(wafer.block_names(), vec!["test/alpha", "test/zeta"]);
}

#[test]
fn block_names_empty_runtime() {
    let wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    assert!(wafer.block_names().is_empty());
}
