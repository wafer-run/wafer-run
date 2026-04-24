//! Host-side static block registration collected via the `inventory` crate.
//!
//! The `#[wafer_block]` proc-macro emits one of these entries per annotated
//! block, gated on `cfg(not(target_arch = "wasm32"))` so WASM guest builds
//! don't carry the machinery. The collection is harvested at startup by
//! `WaferBuilder` (landing in PR γ).

use std::sync::Arc;

use crate::block::Block;

/// One record per `#[wafer_block]`-annotated native block.
///
/// - `name` is the `{org}/{block}` identifier passed to `#[wafer_block]`.
/// - `factory` is a zero-arg constructor building an `Arc<dyn Block>`. The
///   annotated type must expose `fn new() -> Self` and must implement
///   `Block` — the macro emits `|| Arc::new(<Ty>::new()) as Arc<dyn Block>`.
pub struct StaticBlockRegistration {
    pub name: &'static str,
    pub factory: fn() -> Arc<dyn Block>,
}

// Only one `collect!` call per type per binary is allowed; wafer-run owns
// it so consumer crates don't need to repeat it.
inventory::collect!(StaticBlockRegistration);
