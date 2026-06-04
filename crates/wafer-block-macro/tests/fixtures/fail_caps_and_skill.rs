//! `capabilities(...)` and `skill(...)` are mutually exclusive — declaring both
//! on the same block must be rejected with a spanned error rather than silently
//! honoring both.

use wafer_block_macro::wafer_block;

struct Widget;

impl Widget {
    fn new() -> Self {
        Self
    }
}

#[wafer_block(
    name = "acme/widget",
    version = "0.1.0",
    interface = "handler@v1",
    summary = "Test widget",
    capabilities(network),
    skill(
        description = "does a thing",
        parameters = r#"{ "type": "object" }"#
    )
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
