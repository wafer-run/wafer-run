//! Verifies `#[wafer_block]` expands cleanly for a native (host-arch) build
//! when the annotated struct has both a `new()` constructor AND an
//! `impl Block` implementation.

use wafer_block_macro::wafer_async_trait;
use std::sync::Arc;

use wafer_block::{
    block::Block,
    context::Context,
    core_types::Message,
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
};
use wafer_block_macro::wafer_block;
use wafer_block::STATIC_BLOCK_REGISTRATIONS;

struct Widget;

impl Widget {
    fn new() -> Self {
        Self
    }
}

#[wafer_async_trait]
impl Block for Widget {
    fn info(&self) -> BlockInfo {
        <Widget>::block_info()
    }
    async fn handle(
        &self,
        _ctx: &dyn Context,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::drop_request()
    }
}

#[wafer_block(
    name = "acme/widget",
    version = "0.1.0",
    interface = "handler@v1",
    summary = "Test widget"
)]
impl Widget {
    fn handle(_msg: Message, _body: Vec<u8>) -> wafer_sdk::core_abi::GuestResult {
        wafer_sdk::core_abi::GuestResult::respond(vec![])
    }
}

fn main() {
    // linkme entry exists at link time.
    let found = STATIC_BLOCK_REGISTRATIONS
        .iter()
        .any(|r| r.name == "acme/widget");
    assert!(found, "linkme entry missing");

    // Factory produces an Arc<dyn Block>.
    let reg = STATIC_BLOCK_REGISTRATIONS
        .iter()
        .find(|r| r.name == "acme/widget")
        .unwrap();
    let _block: Arc<dyn Block> = (reg.factory)();
}
