//! Shared types, traits, and macros for WAFER block authors.
//!
//! This crate provides the core types used by both the WAFER runtime (`wafer-run`)
//! and the WASM guest SDK (`wafer-sdk`). Block authors depend on this crate for
//! type definitions, and optionally use the `#[wafer_block]` proc macro for
//! reduced boilerplate.

pub mod core_types;
pub mod meta;
pub mod types;

// Re-export the proc macro.
pub use wafer_block_macro::wafer_block;

// Re-export core types at crate root.
pub use core_types::ErrorCode;
pub use core_types::{InstanceMode, LifecycleEvent, LifecycleType, Message, MetaEntry, WaferError};
pub use meta::*;

// Re-export runtime-specific types (needed by block authors for BlockInfo etc.)
pub use types::{
    ActionSpec, AuthLevel, BlockCategory, BlockConfigKey, BlockEndpoint, BlockInfo, BlockRuntime,
    CollectionSchema, ConfigVar, FieldSchema, HttpMethod, IndexSchema, InputType, InterfaceSpec,
    MetaAccess, RequestAction, ResourceGrant, ResourceType, UiRoute,
};

// The following modules are only available on non-WASM targets (they require tokio).
#[cfg(not(target_arch = "wasm32"))]
pub mod block;
#[cfg(not(target_arch = "wasm32"))]
pub mod capabilities;
#[cfg(not(target_arch = "wasm32"))]
pub mod common;
#[cfg(not(target_arch = "wasm32"))]
pub mod compat;
#[cfg(not(target_arch = "wasm32"))]
pub mod config;
#[cfg(not(target_arch = "wasm32"))]
pub mod context;
#[cfg(not(target_arch = "wasm32"))]
pub mod executor;
#[cfg(not(target_arch = "wasm32"))]
pub mod hash;
#[cfg(not(target_arch = "wasm32"))]
pub mod helpers;
#[cfg(not(target_arch = "wasm32"))]
pub mod interfaces;
#[cfg(not(target_arch = "wasm32"))]
pub mod registry;
#[cfg(not(target_arch = "wasm32"))]
pub mod router;
#[cfg(not(target_arch = "wasm32"))]
pub mod stream;
#[cfg(not(target_arch = "wasm32"))]
pub mod streams;
#[cfg(not(target_arch = "wasm32"))]
pub mod wrap;

#[cfg(not(target_arch = "wasm32"))]
pub use block::Block;
#[cfg(not(target_arch = "wasm32"))]
pub use capabilities::BlockCapabilities;
#[cfg(not(target_arch = "wasm32"))]
pub use common::{ServiceName, ServiceOp};
#[cfg(not(target_arch = "wasm32"))]
pub use compat::{MaybeSend, MaybeSync};
#[cfg(not(target_arch = "wasm32"))]
pub use config::{BlockConfig, DispatchTarget};
#[cfg(not(target_arch = "wasm32"))]
pub use context::Context;
#[cfg(not(target_arch = "wasm32"))]
pub use executor::{extract_path_vars, match_path, matches_pattern};
#[cfg(not(target_arch = "wasm32"))]
pub use hash::expand_env_vars;
#[cfg(not(target_arch = "wasm32"))]
pub use hash::{hex_encode, sha256, sha256_hex};
#[cfg(not(target_arch = "wasm32"))]
pub use helpers::*;
#[cfg(not(target_arch = "wasm32"))]
pub use registry::BlockRegistry;
#[cfg(not(target_arch = "wasm32"))]
pub use router::Router;
#[cfg(not(target_arch = "wasm32"))]
pub use stream::StreamEvent;
#[cfg(not(target_arch = "wasm32"))]
pub use streams::input::InputStream;
#[cfg(not(target_arch = "wasm32"))]
pub use streams::output::{
    BufferedResponse, OutputSink, OutputStream, SinkClosed, TerminalNotResponse,
};
