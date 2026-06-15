//! Test that `#[wafer_block(skill(description, parameters))]` produces the
//! expected `BlockInfo::tool` value.
//!
//! Each block is placed in its own module to avoid symbol clashes for the
//! generated `#[no_mangle]` ABI exports. Tests call the generated
//! `block_info()` associated function which returns `BlockInfo` directly
//! without the WASM ABI overhead.

use wafer_block::Message;
use wafer_block_macro::{wafer_async_trait, wafer_block};
use wafer_sdk::core_abi::GuestResult;

mod skill_block {
    use super::*;

    pub struct AddNumbers;

    #[wafer_block(
        name = "test/add-numbers",
        version = "0.1.0",
        interface = "handler@v1",
        summary = "Adds two numbers",
        skill(
            description = "Add two integers and return the sum.",
            parameters = r#"{
                "type": "object",
                "properties": {
                    "a": { "type": "integer" },
                    "b": { "type": "integer" }
                },
                "required": ["a", "b"]
            }"#
        )
    )]
    impl AddNumbers {
        fn handle(_msg: Message, _body: Vec<u8>) -> GuestResult {
            GuestResult::respond(b"{}".to_vec())
        }
    }

    impl AddNumbers {
        pub fn new() -> Self {
            Self
        }
    }

    #[wafer_async_trait]
    impl wafer_block::block::Block for AddNumbers {
        fn info(&self) -> wafer_block::types::BlockInfo {
            Self::block_info()
        }
        async fn handle(
            &self,
            _ctx: &dyn wafer_block::context::Context,
            _msg: wafer_block::core_types::Message,
            _input: wafer_block::streams::input::InputStream,
        ) -> wafer_block::streams::output::OutputStream {
            wafer_block::streams::output::OutputStream::drop_request()
        }
    }
}

mod non_skill_block {
    use super::*;

    pub struct PlainBlock;

    #[wafer_block(
        name = "test/plain",
        version = "0.1.0",
        interface = "handler@v1",
        summary = "A non-skill block"
    )]
    impl PlainBlock {
        fn handle(_msg: Message, _body: Vec<u8>) -> GuestResult {
            GuestResult::respond(b"{}".to_vec())
        }
    }

    impl PlainBlock {
        pub fn new() -> Self {
            Self
        }
    }

    #[wafer_async_trait]
    impl wafer_block::block::Block for PlainBlock {
        fn info(&self) -> wafer_block::types::BlockInfo {
            Self::block_info()
        }
        async fn handle(
            &self,
            _ctx: &dyn wafer_block::context::Context,
            _msg: wafer_block::core_types::Message,
            _input: wafer_block::streams::input::InputStream,
        ) -> wafer_block::streams::output::OutputStream {
            wafer_block::streams::output::OutputStream::drop_request()
        }
    }
}

#[test]
fn skill_attribute_sets_tool() {
    use skill_block::AddNumbers;
    let info = AddNumbers::block_info();
    let tool = info.tool.expect("skill(...) attribute must set tool");
    assert_eq!(tool.description, "Add two integers and return the sum.");
    assert_eq!(tool.parameters["type"], "object");
    assert_eq!(tool.parameters["properties"]["a"]["type"], "integer");
    assert_eq!(tool.parameters["required"], serde_json::json!(["a", "b"]));
}

#[test]
fn no_skill_attribute_leaves_tool_unset() {
    use non_skill_block::PlainBlock;
    let info = PlainBlock::block_info();
    assert!(
        info.tool.is_none(),
        "block without skill(...) must have tool = None"
    );
}
