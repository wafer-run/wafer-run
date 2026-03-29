//! Shared types, traits, and macros for WAFER block authors.
//!
//! This crate provides the core types used by both the WAFER runtime (`wafer-run`)
//! and the WASM guest SDK (`wafer-sdk`). Block authors depend on this crate for
//! type definitions, and optionally use the `#[wafer_block]` proc macro for
//! reduced boilerplate.

pub mod block;
pub mod capabilities;
pub mod common;
pub mod compat;
pub mod config;
pub mod context;
pub mod executor;
pub mod hash;
pub mod helpers;
pub mod meta;
pub mod registry;
pub mod router;
pub mod types;

// Re-export everything at the crate root for convenience.
pub use block::{AsyncFuncBlock, Block, FuncBlock};
pub use capabilities::BlockCapabilities;
pub use common::{ErrorCode, ServiceName, ServiceOp};
pub use compat::{MaybeSend, MaybeSync};
pub use config::{BlockConfig, DispatchTarget};
pub use context::Context;
pub use executor::{extract_path_vars, match_path, matches_pattern};
pub use hash::{sha256, sha256_hex, hex_encode};
#[cfg(not(target_arch = "wasm32"))]
pub use hash::expand_env_vars;
pub use helpers::*;
pub use meta::*;
pub use registry::BlockRegistry;
pub use router::Router;

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
    Action, BlockResult, InstanceMode, LifecycleEvent, LifecycleType, Message,
    MetaEntry, Response, WaferError,
};

// Re-export the WIT Guest trait for WASM block authors.
pub use exports::wafer::block_world::block::Guest;

/// WIT runtime host imports — only usable from WASM components.
///
/// These functions trap to the host runtime for inter-block calls, cancellation
/// checks, and logging. They are only available when targeting `wasm32`.
#[cfg(target_arch = "wasm32")]
pub mod runtime {
    pub use super::wafer::block_world::runtime::{call_block, is_cancelled, log};
}

// Re-export runtime-specific types.
pub use types::{AdminUIInfo, BlockInfo, BlockRuntime, CollectionSchema, FieldSchema, IndexSchema, MetaAccess, RequestAction};

/// Alias for BlockResult — common in block handler return types.
pub type Result_ = BlockResult;
