//! Static block registration: the record type, and the `linkme` slice that
//! collects it where a linker section exists.
//!
//! `linkme` uses an ELF section that the linker preserves even when no
//! code-level reference exists from the consumer binary, unlike `inventory`
//! which gets linker-DCE'd for standalone crates whose only consumer reference
//! was a `pub fn register()` call.
//!
//! The `#[wafer_block]` proc-macro emits one of these entries per annotated
//! block, gated on `cfg(not(target_arch = "wasm32"))` so WASM guest builds
//! don't carry the machinery. The collection is harvested at startup by
//! the runtime (`wafer-run/src/builder.rs`).
//!
//! [`StaticBlockRegistration`] itself is target-neutral — it is a name and a
//! constructor, nothing else. Only the *collection* is linker-shaped, so only
//! [`STATIC_BLOCK_REGISTRATIONS`] is gated. On `wasm32` there is no section to
//! collect into, and the entries travel explicitly instead: see
//! [`register_static_block!`](crate::register_static_block) and
//! [`use_static_blocks!`](crate::use_static_blocks).

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
///
/// Absent on `wasm32`, where `linkme`'s link sections do not exist. A wasm32
/// embedder gets the same entries from the `WAFER_STATIC_BLOCKS` list that
/// [`use_static_blocks!`](crate::use_static_blocks) emits.
#[cfg(not(target_arch = "wasm32"))]
#[linkme::distributed_slice]
pub static STATIC_BLOCK_REGISTRATIONS: [StaticBlockRegistration] = [..];
