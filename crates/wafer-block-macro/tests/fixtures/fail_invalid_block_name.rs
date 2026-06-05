//! `name` is a single-segment identifier — the macro must reject it with a
//! spanned error instead of emitting a block with an invalid name.

use wafer_block_macro::wafer_block;

struct Widget;

impl Widget {
    fn new() -> Self {
        Self
    }
}

#[wafer_block(
    name = "widget",
    version = "0.1.0",
    interface = "handler@v1",
    summary = "Test widget"
)]
impl Widget {
    fn handle(
        _msg: wafer_block::core_types::Message,
        _body: Vec<u8>,
    ) -> wafer_sdk::core_abi::GuestResult {
        wafer_sdk::core_abi::GuestResult::respond(vec![])
    }
}

fn main() {}
