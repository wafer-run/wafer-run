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
//!     fn handle(msg: Message) -> BlockResult {
//!         msg.cont()
//!     }
//! }
//! ```

pub mod core_abi;
pub mod pure;

// Re-export everything from wafer-block (types, traits, helpers, macros).
pub use wafer_block::*;

// Re-export runtime functions for block authors.
pub use core_abi::{call_block, is_cancelled, log};
