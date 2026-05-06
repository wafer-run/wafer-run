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
#[cfg(target_arch = "wasm32")]
pub mod clients;
pub mod core_abi;
pub mod pure;
pub mod stream;

// Re-export everything from wafer-block (types, traits, helpers, macros).
// Re-export runtime functions and guest result types for block authors.
pub use attachment::lookup_attachment;
pub use core_abi::{is_cancelled, log, GuestResponse, GuestResult};
pub use wafer_block::*;
