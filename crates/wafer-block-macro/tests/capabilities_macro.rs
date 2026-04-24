//! Test that #[wafer_block(capabilities(...))] produces the expected
//! BlockInfo::capabilities value.
//!
//! Each block is placed in its own module to avoid symbol clashes for the
//! generated `#[no_mangle]` ABI exports (`__wafer_info`, `__wafer_handle`,
//! `__wafer_lifecycle`). Tests call the generated `block_info()` associated
//! function which returns `BlockInfo` directly without the WASM ABI overhead.
//!
//! NOTE: These tests are gated to `cfg(target_arch = "wasm32")` because the
//! host-side expansion of `#[wafer_block]` now emits a
//! `::wafer_run::inventory::submit!` entry which requires the annotated type
//! to expose `fn new() -> Self` and implement `::wafer_run::Block`, plus the
//! test crate to depend on `wafer-run`. That would introduce a dependency
//! cycle (`wafer-run` already depends on `wafer-block-macro`), so the
//! host-path coverage for block metadata has been moved to the `wafer-run`
//! integration tests instead.

#![cfg(target_arch = "wasm32")]

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
