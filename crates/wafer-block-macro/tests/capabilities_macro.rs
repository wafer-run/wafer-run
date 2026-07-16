//! Test that #[wafer_block(capabilities(...))] produces the expected
//! BlockInfo::capabilities value.
//!
//! Each block is placed in its own module to avoid symbol clashes for the
//! generated `#[no_mangle]` ABI exports (`__wafer_info`, `__wafer_handle`,
//! `__wafer_lifecycle`). Tests call the generated `block_info()` associated
//! function which returns `BlockInfo` directly without the WASM ABI overhead.

use wafer_block::Message;
use wafer_block_macro::{wafer_async_trait, wafer_block};
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
            ddl,
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
        #[expect(
            clippy::needless_pass_by_value,
            reason = "macro-required `handle` signature: #[wafer_block] re-emits this fn verbatim with by-value params"
        )]
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

    #[wafer_async_trait]
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
    assert_eq!(caps.network, wafer_block::Allowlist::Any);
    assert!(!caps.raw_sql);
    assert!(caps.ddl);
    assert_eq!(caps.config, wafer_block::Allowlist::None);
    assert!(caps.collections.allows("users"));
    assert!(caps.collections.allows("sessions"));
    assert!(caps.callable_blocks.allows("wafer-run/crypto"));
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
fn undeclared_has_no_capabilities() {
    use undeclared_block::Undeclared;
    let info = Undeclared::block_info();
    assert!(
        info.capabilities.is_none(),
        "block without capabilities(...) should have None"
    );
}

// ---------------------------------------------------------------------------
// Wildcard-list fields: `["*"]` (and `["*", x]`) map to `Allowlist::Any`,
// preserving the pre-enum "*" = all sentinel through the macro DSL.
// ---------------------------------------------------------------------------

mod wildcard_block {
    use super::*;

    pub struct Wildcard;

    #[wafer_block(
        name = "test/wildcard",
        version = "0.1.0",
        interface = "middleware@v1",
        summary = "test",
        capabilities(
            collections = ["*"],
            // A list that ALSO carries a specific entry still collapses to Any
            // (matching the old HashSet where "*" short-circuited membership).
            callable_blocks = ["*", "specific/block"],
            vector_indexes = ["my_org__vector__docs"],
        )
    )]
    impl Wildcard {
        #[expect(
            clippy::needless_pass_by_value,
            reason = "macro-required `handle` signature: #[wafer_block] re-emits this fn verbatim with by-value params"
        )]
        fn handle(msg: Message, _body: Vec<u8>) -> GuestResult {
            let _ = msg;
            GuestResult::respond(b"{}".to_vec())
        }
    }

    impl Wildcard {
        pub fn new() -> Self {
            Self
        }
    }

    #[wafer_async_trait]
    impl wafer_block::block::Block for Wildcard {
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
fn wildcard_list_maps_to_any() {
    use wafer_block::Allowlist;
    use wildcard_block::Wildcard;
    let caps = Wildcard::block_info().capabilities.expect("caps present");
    assert_eq!(caps.collections, Allowlist::Any, "[\"*\"] -> Any");
    assert_eq!(
        caps.callable_blocks,
        Allowlist::Any,
        "[\"*\", x] -> Any (wildcard short-circuits)"
    );
    // A plain list still becomes a restricted Only.
    assert_eq!(
        caps.vector_indexes,
        Allowlist::Only(
            ["my_org__vector__docs"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        )
    );
}
