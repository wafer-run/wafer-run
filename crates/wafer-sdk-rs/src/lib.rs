//! WAFER guest SDK for writing blocks compiled to WebAssembly.
//!
//! This crate provides the types and traits needed to implement a WASM
//! block using the Component Model. Types come from `wafer-block` (WIT-generated)
//! and are re-exported here for convenience.
//!
//! # Quick start
//!
//! ```rust,ignore
//! use wafer_sdk::*;
//!
//! struct MyBlock;
//!
//! #[wafer_block(
//!     name = "my-block",
//!     version = "0.1.0",
//!     interface = "transform",
//!     summary = "A demo block"
//! )]
//! impl MyBlock {
//!     fn handle(msg: Message) -> BlockResult {
//!         msg.cont()
//!     }
//! }
//! ```

// Re-export everything from wafer-block (types, traits, helpers, macros).
pub use wafer_block::*;
