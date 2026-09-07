//! Compile fixture for the `wasm32` arms of `register_static_block!` and
//! `use_static_blocks!`. See this crate's `Cargo.toml` for why it exists and
//! how it is built.
//!
//! The invocation below is the only thing in the tree that expands
//! `use_static_blocks!` on a target where `linkme` is unavailable, so it is
//! what proves that each named crate really does define a
//! `__WAFER_STATIC_BLOCK` of the right type and that the emitted
//! `WAFER_STATIC_BLOCKS` list typechecks.
//!
//! The native half of the same contract — that the list is empty wherever
//! `linkme` works, so the call site needs no `cfg` — is pinned by
//! `crates/wafer-run/tests/static_block_registration.rs`.

wafer_block::use_static_blocks!(
    wafer_block_cors,
    wafer_block_inspector,
    wafer_block_readonly_guard,
    wafer_block_router,
    wafer_block_security_headers,
    wafer_block_web,
);

/// One entry per named crate on `wasm32`, and none anywhere else.
///
/// A `wasm32` arm that expanded to an empty list would compile green and
/// reproduce the exact bug the arm exists to fix — a wasm32 runtime whose
/// middleware registry is silently empty — so the count is asserted, not
/// merely typechecked. `WAFER_STATIC_BLOCKS` is a `const`, which is what
/// makes it readable from this const context.
const _: () = assert!(
    WAFER_STATIC_BLOCKS.len() == if cfg!(target_arch = "wasm32") { 6 } else { 0 },
    "use_static_blocks! must hand over every named crate's block on wasm32, \
     and nothing at all where linkme already collected them"
);

/// The blocks this crate's `use_static_blocks!` hands over explicitly,
/// because link-time collection could not reach them on this target.
///
/// Reads the list at run time as well as in the assertion above, so the
/// entries have to survive to the final artifact and not merely typecheck.
pub fn explicit_block_names() -> Vec<&'static str> {
    WAFER_STATIC_BLOCKS.iter().map(|r| r.name).collect()
}
