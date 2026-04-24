//! Struct lacks `new()` — macro expansion's `<Widget>::new()` factory
//! reference must fail to compile.

use wafer_block::core_types::Message;
use wafer_block_macro::wafer_block;

struct Widget;

// Intentionally no `impl Widget { fn new() -> Self { ... } }` and no
// `impl Block for Widget`.

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

fn main() {}
