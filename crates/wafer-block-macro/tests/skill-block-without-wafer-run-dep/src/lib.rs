//! Regression fixture: a minimal skill-block crate that uses `#[wafer_block]`
//! and depends only on `wafer-sdk` + `wafer-block` (NOT `wafer-run`).
//!
//! Compiled for `wasm32-wasip1` by the
//! `skill_block_without_wafer_run_dep` integration test, which asserts the
//! build succeeds. Before the cfg-gate fix in `wafer-block-macro`, this would
//! fail with `error[E0433]: failed to resolve: could not find 'wafer_run'`.
//!
//! Declares `skill(...)` + `capabilities(...)` together to also lock the
//! composability contract in this minimal dep graph: the capability tokens
//! the macro emits (`wafer_block::BlockCapabilities` et al.) must resolve
//! through the `wafer_sdk` re-export, without `wafer-run` in the graph.

use wafer_sdk::*;

struct Probe;

#[wafer_block(
    name = "test/probe",
    version = "0.1.0",
    interface = "handler@v1",
    summary = "Regression probe for wasm32 macro expansion without wafer-run dep",
    skill(
        description = "Probe tool for macro-expansion regression coverage.",
        parameters = r#"{ "type": "object" }"#
    ),
    capabilities(network, callable_blocks = ["wafer-run/network"])
)]
impl Probe {
    fn handle(_msg: Message, _body: Vec<u8>) -> GuestResult {
        let bytes = serde_json::to_vec(&serde_json::json!({"ok": true})).unwrap_or_default();
        GuestResult::respond(bytes)
    }
}
