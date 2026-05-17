//! Host-side static block registration collected via `linkme`.
//!
//! `linkme` uses an ELF section that the linker preserves even when no
//! code-level reference exists from the consumer binary, unlike `inventory`
//! which gets linker-DCE'd for standalone crates whose only consumer reference
//! was a `pub fn register()` call.
//!
//! The `#[wafer_block]` proc-macro emits one of these entries per annotated
//! block, gated on `cfg(not(target_arch = "wasm32"))` so WASM guest builds
//! don't carry the machinery. The collection is harvested at startup by
//! `WaferBuilder`.

use std::sync::Arc;

use crate::block::Block;

/// One record per `#[wafer_block]`-annotated native block.
///
/// - `name` is the `{org}/{block}` identifier passed to `#[wafer_block]`.
/// - `factory` is a zero-arg constructor building an `Arc<dyn Block>`. The
///   annotated type must expose `fn new() -> Self` and must implement
///   `Block` — the macro emits `|| Arc::new(<Ty>::new()) as Arc<dyn Block>`.
pub struct StaticBlockRegistration {
    /// `{org}/{block}` identifier supplied to `#[wafer_block]`.
    pub name: &'static str,
    /// Zero-arg constructor that materialises the block as `Arc<dyn Block>`.
    pub factory: fn() -> Arc<dyn Block>,
}

/// The link-time distributed slice that collects every `StaticBlockRegistration`
/// contributed by any crate linked into the final binary.
///
/// Consumer crates must use the `register_static_block!` macro rather than
/// touching this slice directly.
#[linkme::distributed_slice]
pub static STATIC_BLOCK_REGISTRATIONS: [StaticBlockRegistration] = [..];
