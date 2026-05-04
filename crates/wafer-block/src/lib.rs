//! Shared types, traits, and macros for WAFER block authors.
//!
//! This crate provides the core types used by both the WAFER runtime (`wafer-run`)
//! and the WASM guest SDK (`wafer-sdk`). Block authors depend on this crate for
//! type definitions, and optionally use the `#[wafer_block]` proc macro for
//! reduced boilerplate.

pub mod codec;
pub mod core_types;
pub mod error;
pub mod meta;
pub mod types;

// Re-export the proc macro.
// Re-export core types at crate root.
pub use core_types::{
    ErrorCode, InstanceMode, LifecycleEvent, LifecycleType, Message, MetaEntry, WaferError,
};
pub use meta::*;
// Re-export runtime-specific types (needed by block authors for BlockInfo etc.)
pub use types::{
    ActionSpec, AuthLevel, BlockCategory, BlockConfigKey, BlockEndpoint, BlockInfo, BlockRuntime,
    CollectionSchema, ConfigVar, ExternalAsset, FieldSchema, HttpMethod, IndexSchema, InputType,
    InterfaceSpec, MetaAccess, RequestAction, ResourceGrant, ResourceType, SkillRole, SkillTool,
    UiRoute,
};
pub use wafer_block_macro::wafer_block;

// All modules below are now wasm32-compatible: streams use
// `spawn_producer` (tokio::spawn on native, spawn_local on wasm32), and other
// modules either never used tokio or use only wasm32-safe tokio primitives
// (channels, CancellationToken).
pub mod block;
pub mod capabilities;
pub mod common;
pub mod compat;
pub mod config;
pub mod context;
pub mod executor;
pub mod hash;
pub mod helpers;
pub mod interfaces;
pub mod registry;
pub mod router;
pub mod spawn;
pub mod stream;
pub mod streams;
pub mod wrap;

pub use block::Block;
pub use capabilities::BlockCapabilities;
pub use common::{ServiceName, ServiceOp};
pub use compat::{MaybeSend, MaybeSync};
pub use config::{BlockConfig, DispatchTarget};
pub use context::Context;
pub use error::RuntimeError;
pub use executor::{extract_path_vars, match_path, matches_pattern};
#[cfg(not(target_arch = "wasm32"))]
pub use hash::expand_env_vars;
pub use hash::{hex_encode, sha256, sha256_hex};
pub use helpers::*;
pub use registry::BlockRegistry;
pub use router::Router;
pub use spawn::spawn_producer;
pub use stream::StreamEvent;
pub use streams::{
    input::InputStream,
    output::{BufferedResponse, OutputSink, OutputStream, SinkClosed, TerminalNotResponse},
};
