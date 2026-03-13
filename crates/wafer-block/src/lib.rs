//! Shared types, traits, and macros for WAFER block authors.
//!
//! This crate provides the core types used by both the WAFER runtime (`wafer-run`)
//! and the WASM guest SDK (`wafer-sdk`). Block authors depend on this crate for
//! type definitions, and optionally use the `#[wafer_block]` proc macro for
//! reduced boilerplate.

pub mod helpers;
pub mod meta;
pub mod types;

// Re-export everything at the crate root for convenience.
pub use helpers::*;
pub use meta::*;

// Re-export the proc macro.
pub use wafer_block_macro::wafer_block;

wit_bindgen::generate!({
    world: "wafer-block",
    path: "../../wit/wit",
    pub_export_macro: true,
    export_macro_name: "export_wafer_block",
    additional_derives: [serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash],
});

// Re-export WIT types at crate root (skip WIT BlockInfo — runtime version is in types.rs).
pub use wafer::block_world::types::{
    Action, BlockResult, ErrorCode, InstanceMode, LifecycleEvent, LifecycleType, Message,
    MetaEntry, Response, WaferError,
};

// Re-export the WIT Guest trait for WASM block authors.
pub use exports::wafer::block_world::block::Guest;

// Re-export runtime-specific types.
pub use types::{AdminUIInfo, BlockInfo, BlockRuntime, MetaAccess, RequestAction};

/// Alias for BlockResult — common in block handler return types.
pub type Result_ = BlockResult;
