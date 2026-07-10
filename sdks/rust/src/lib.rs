#![warn(missing_docs)]
//! WAFER guest SDK for writing blocks compiled to WebAssembly.
//!
//! This crate provides the types and traits needed to implement a WASM
//! block using the wasmi + JSON ABI. Types come from `wafer-block` and are
//! re-exported here for convenience. Host-import wrappers are in [`core_abi`].
//!
//! # Quick start
//!
//! ```rust,ignore
//! use wafer_sdk::*;
//!
//! struct MyBlock;
//!
//! impl MyBlock {
//!     async fn handle(msg: Message, input: InputStream) -> OutputStream {
//!         OutputStream::continue_with(msg)
//!     }
//! }
//! ```

pub mod attachment;
// Client wrappers call WASM host imports, so the module is guest-only in
// normal builds; it additionally compiles under native `cargo test` (the
// stream layer has native stubs) so in-module drift tests — e.g. the
// database client's DATABASE_OPS coverage guard — run in ordinary CI.
#[cfg(any(target_arch = "wasm32", test))]
pub mod clients;
pub mod core_abi;
pub mod stream;

// Re-export everything from wafer-block (types, traits, helpers, macros).
// Re-export runtime functions and guest result types for block authors.
pub use attachment::lookup_attachment;
pub use core_abi::{is_cancelled, log, GuestResponse, GuestResult};
pub use wafer_block::*;
