//! Test that #[wafer_block(capabilities(...))] produces the expected
//! BlockInfo::capabilities value.
//!
//! Each block is placed in its own module to avoid symbol clashes for the
//! generated `#[no_mangle]` ABI exports (`__wafer_info`, `__wafer_handle`,
//! `__wafer_lifecycle`). Tests call the generated `block_info()` associated
//! function which returns `BlockInfo` directly without the WASM ABI overhead.

use wafer_block::Message;
use wafer_block_macro::wafer_block;
use wafer_sdk::core_abi::GuestResult;

// ---------------------------------------------------------------------------
// Block with a full capabilities declaration
// ---------------------------------------------------------------------------

mod fully_declared_block {
    use super::*;

    pub struct FullyDeclared;

    #[wafer_block(
        name = "test/fully-declared",
        version = "0.1.0",
        interface = "middleware@v1",
        summary = "test",
        capabilities(
            crypto,
            network,
            collections = ["users", "sessions"],
            callable_blocks = ["wafer-run/crypto"],
            headers(
                readable = ["authorization"],
                writable = ["set-cookie"],
                masked = ["x-internal"],
            ),
        )
    )]
    impl FullyDeclared {
        fn handle(msg: Message, _body: Vec<u8>) -> GuestResult {
            let _ = msg;
            GuestResult::respond(b"{}".to_vec())
        }
    }

    impl FullyDeclared {
        pub fn new() -> Self {
            Self
        }
    }

    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    impl wafer_block::block::Block for FullyDeclared {
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
fn fully_declared_caps_present() {
    use fully_declared_block::FullyDeclared;
    let info = FullyDeclared::block_info();
    let caps = info.capabilities.expect("caps should be present");
    assert!(caps.crypto);
    assert!(caps.network);
    assert!(!caps.raw_sql);
    assert!(!caps.config);
    assert!(caps.collections.contains("users"));
    assert!(caps.collections.contains("sessions"));
    assert!(caps.callable_blocks.contains("wafer-run/crypto"));
    assert_eq!(caps.headers.readable, vec!["authorization".to_string()]);
    assert_eq!(caps.headers.writable, vec!["set-cookie".to_string()]);
    assert_eq!(caps.headers.masked, vec!["x-internal".to_string()]);
}

// ---------------------------------------------------------------------------
// Block without a capabilities declaration
// ---------------------------------------------------------------------------

mod undeclared_block {
    use super::*;

    pub struct Undeclared;

    #[wafer_block(
        name = "test/undeclared",
        version = "0.1.0",
        interface = "middleware@v1",
        summary = "test"
    )]
    impl Undeclared {
        fn handle(msg: Message, _body: Vec<u8>) -> GuestResult {
            let _ = msg;
            GuestResult::respond(b"{}".to_vec())
        }
    }

    impl Undeclared {
        pub fn new() -> Self {
            Self
        }
    }

    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    impl wafer_block::block::Block for Undeclared {
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
fn undeclared_has_no_capabilities() {
    use undeclared_block::Undeclared;
    let info = Undeclared::block_info();
    assert!(
        info.capabilities.is_none(),
        "block without capabilities(...) should have None"
    );
}
