//! Test that `#[wafer_block(requires = [...])]` reaches `BlockInfo::requires`.
//!
//! `requires` declares which other blocks this block may `call_block`; the
//! runtime enforces it as an access-control gate. Regression: the attribute was
//! parsed but bound to an underscore-discarded variable and never wired into the
//! generated `block_info()`, so a declared list silently produced an empty
//! (unrestricted) `requires`.
//!
//! Each block lives in its own module to avoid `#[no_mangle]` ABI symbol clashes.

use wafer_block::Message;
use wafer_block_macro::{wafer_async_trait, wafer_block};
use wafer_sdk::core_abi::GuestResult;

mod declared_block {
    use super::*;

    pub struct Declared;

    #[wafer_block(
        name = "test/declares-requires",
        version = "0.1.0",
        interface = "middleware@v1",
        summary = "test",
        requires = ["wafer-run/database", "wafer-run/crypto"]
    )]
    impl Declared {
        #[expect(
            clippy::needless_pass_by_value,
            reason = "macro-required `handle` signature: #[wafer_block] re-emits this fn verbatim with by-value params"
        )]
        fn handle(msg: Message, _body: Vec<u8>) -> GuestResult {
            let _ = msg;
            GuestResult::respond(b"{}".to_vec())
        }
    }

    impl Declared {
        pub fn new() -> Self {
            Self
        }
    }

    #[wafer_async_trait]
    impl wafer_block::block::Block for Declared {
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
fn declared_requires_reaches_block_info() {
    use declared_block::Declared;
    let info = Declared::block_info();
    assert_eq!(
        info.requires,
        vec![
            "wafer-run/database".to_string(),
            "wafer-run/crypto".to_string(),
        ],
        "declared `requires` list must reach BlockInfo::requires"
    );
}

mod undeclared_block {
    use super::*;

    pub struct Undeclared;

    #[wafer_block(
        name = "test/no-requires",
        version = "0.1.0",
        interface = "middleware@v1",
        summary = "test"
    )]
    impl Undeclared {
        #[expect(
            clippy::needless_pass_by_value,
            reason = "macro-required `handle` signature: #[wafer_block] re-emits this fn verbatim with by-value params"
        )]
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

    #[wafer_async_trait]
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
fn undeclared_requires_is_empty() {
    use undeclared_block::Undeclared;
    let info = Undeclared::block_info();
    assert!(
        info.requires.is_empty(),
        "block without `requires` should have an empty requires list"
    );
}
