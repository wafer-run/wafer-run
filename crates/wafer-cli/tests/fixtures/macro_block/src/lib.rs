//! A block exactly as a block author writes it: one `#[wafer_block]` impl,
//! no hand-written ABI glue. It echoes the request body and declares a
//! config-key capability, so its `BlockInfo` carries a nested value through
//! the codec, not only strings.
//!
//! `crates/wafer-cli/tests/core_abi.rs` runs `wafer package` and `wafer test`
//! against it: whatever codec the macro emits is the one the CLI must speak.

use wafer_sdk::*;

struct MacroBlock;

#[wafer_block(
    name = "acme/macro-block",
    version = "0.1.0",
    interface = "handler@v1",
    summary = "Echoes its body (wafer CLI fixture)",
    capabilities(config, config_keys = ["ACME__MACRO_BLOCK__GREETING"])
)]
impl MacroBlock {
    fn handle(_msg: Message, body: Vec<u8>) -> GuestResult {
        GuestResult::respond(body)
    }
}
