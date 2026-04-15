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
pub mod core_types;
pub mod executor;
pub mod hash;
pub mod helpers;
pub mod interfaces;
pub mod meta;
pub mod registry;
pub mod router;
pub mod stream;
pub mod streams;
pub mod types;
pub mod wrap;

// Re-export everything at the crate root for convenience.
pub use block::{AsyncFuncBlock, Block, FuncBlock};
pub use capabilities::BlockCapabilities;
pub use common::{ErrorCode, ServiceName, ServiceOp};
pub use compat::{MaybeSend, MaybeSync};
pub use config::{BlockConfig, DispatchTarget};
pub use context::Context;
pub use executor::{extract_path_vars, match_path, matches_pattern};
#[cfg(not(target_arch = "wasm32"))]
pub use hash::expand_env_vars;
pub use hash::{hex_encode, sha256, sha256_hex};
pub use helpers::*;
pub use meta::*;
pub use registry::BlockRegistry;
pub use router::Router;

// Re-export the proc macro.
pub use wafer_block_macro::wafer_block;

// Re-export core types at crate root.
pub use core_types::{
    Action, BlockResult, InstanceMode, LifecycleEvent, LifecycleType, Message, MetaEntry, Response,
    WaferError,
};

// Re-export runtime-specific types.
pub use types::{
    ActionSpec, AuthLevel, BlockCategory, BlockConfigKey, BlockEndpoint, BlockInfo, BlockRuntime,
    CollectionSchema, ConfigVar, FieldSchema, HttpMethod, IndexSchema, InputType, InterfaceSpec,
    MetaAccess, RequestAction, ResourceGrant, ResourceType, UiRoute,
};

/// Alias for BlockResult — common in block handler return types.
pub use core_types::Result_;

pub use stream::StreamEvent;
pub use streams::input::InputStream;
pub use streams::output::{
    BufferedResponse, OutputSink, OutputStream, SinkClosed, TerminalNotResponse,
};
