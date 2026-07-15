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

mod skill_with_caps_block {
    use super::*;

    pub struct FetchTool;

    #[wafer_block(
        name = "test/fetch-tool",
        version = "0.1.0",
        interface = "handler@v1",
        summary = "A skill that fetches input over the network",
        skill(
            description = "Fetch a URL and return its body.",
            parameters = r#"{
                "type": "object",
                "properties": { "url": { "type": "string" } },
                "required": ["url"]
            }"#
        ),
        capabilities(network, callable_blocks = ["wafer-run/network"])
    )]
    impl FetchTool {
        fn handle(_msg: Message, _body: Vec<u8>) -> GuestResult {
            GuestResult::respond(b"{}".to_vec())
        }
    }

    impl FetchTool {
        pub fn new() -> Self {
            Self
        }
    }

    #[wafer_async_trait]
    impl wafer_block::block::Block for FetchTool {
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

// A skill block whose `parameters` is an EXPRESSION (a `const &str`), not a
// string literal — the single-source-descriptor pattern gizza uses. Also
// exercises a `fn -> String` to prove the `AsRef<str>` coercion covers both.
mod skill_block_expr {
    use super::*;

    pub const SCHEMA: &str = r#"{
        "type": "object",
        "properties": { "x": { "type": "integer" } },
        "required": ["x"]
    }"#;

    pub fn schema_owned() -> String {
        SCHEMA.to_string()
    }

    pub struct ConstExprSkill;

    #[wafer_block(
        name = "test/const-expr-skill",
        version = "0.1.0",
        interface = "handler@v1",
        summary = "Schema via const expression",
        skill(
            description = "Schema supplied as a const &str expression.",
            parameters = SCHEMA
        )
    )]
    impl ConstExprSkill {
        fn handle(_msg: Message, _body: Vec<u8>) -> GuestResult {
            GuestResult::respond(b"{}".to_vec())
        }
    }
    impl ConstExprSkill {
        pub fn new() -> Self {
            Self
        }
    }
    #[wafer_async_trait]
    impl wafer_block::block::Block for ConstExprSkill {
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

    pub struct FnExprSkill;

    #[wafer_block(
        name = "test/fn-expr-skill",
        version = "0.1.0",
        interface = "handler@v1",
        summary = "Schema via fn-call expression",
        skill(
            description = "Schema supplied as a fn() -> String expression.",
            parameters = schema_owned()
        )
    )]
    impl FnExprSkill {
        fn handle(_msg: Message, _body: Vec<u8>) -> GuestResult {
            GuestResult::respond(b"{}".to_vec())
        }
    }
    impl FnExprSkill {
        pub fn new() -> Self {
            Self
        }
    }
    #[wafer_async_trait]
    impl wafer_block::block::Block for FnExprSkill {
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
fn skill_parameters_accepts_const_expression() {
    let info = skill_block_expr::ConstExprSkill::block_info();
    let tool = info.tool.expect("skill(...) with const expr must set tool");
    assert_eq!(
        tool.description,
        "Schema supplied as a const &str expression."
    );
    assert_eq!(tool.parameters["type"], "object");
    assert_eq!(tool.parameters["properties"]["x"]["type"], "integer");
    assert_eq!(tool.parameters["required"], serde_json::json!(["x"]));
}

#[test]
fn skill_parameters_accepts_fn_call_expression() {
    let info = skill_block_expr::FnExprSkill::block_info();
    let tool = info.tool.expect("skill(...) with fn expr must set tool");
    assert_eq!(tool.parameters["properties"]["x"]["type"], "integer");
}

/// SEC-02 consumer path: `skill(...)` and `capabilities(...)` compose — a
/// skill that calls a service block declares both the LLM tool contract and
/// the sandbox grants it needs. Under fail-closed defaults an undeclared
/// WASM skill gets `none()`, so this is the only way a skill can hold a
/// service capability.
#[test]
fn skill_and_capabilities_compose() {
    let info = skill_with_caps_block::FetchTool::block_info();

    let tool = info.tool.expect("skill(...) must still set tool");
    assert_eq!(tool.description, "Fetch a URL and return its body.");

    let caps = info
        .capabilities
        .expect("capabilities(...) alongside skill(...) must set capabilities");
    assert!(caps.network, "network flag must be honored");
    assert!(
        caps.callable_blocks.contains("wafer-run/network"),
        "callable_blocks must be honored"
    );
    assert!(!caps.crypto, "undeclared fields stay denied");
}
