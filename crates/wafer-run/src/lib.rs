//! WAFER — WebAssembly Architecture for Flow Execution & Routing
//!
//! A message-processing runtime that executes flows of blocks.
//! Each block receives a message, processes it, and returns a result
//! that determines the next step in the flow.

pub mod block;
pub mod common;
pub mod config;
pub mod context;
pub mod executor;
pub mod helpers;
pub mod manifest;
pub mod meta;
#[cfg(not(target_arch = "wasm32"))]
pub mod observability;
pub mod router;
#[cfg(not(target_arch = "wasm32"))]
pub mod runtime;
pub mod schema;
pub mod security;
pub mod types;
pub mod wasm;

// Re-exports for convenience
pub use block::{AdminUIInfo, Block, BlockInfo};
pub use config::{
    Flow, FlowConfig, FlowConfigDef, FlowDef, FlowInfo, Node,
    NodeDef,
};
pub use context::Context;
#[cfg(not(target_arch = "wasm32"))]
pub use context::RuntimeContext;
pub use executor::{extract_path_vars, match_path, matches_pattern};
pub use helpers::{
    err_bad_request, err_conflict, err_forbidden, err_internal, err_not_found, err_unauthorized,
    err_validation, error, expand_env_vars, json_respond, new_response, respond, respond_empty,
    respond_json, sha256_hex, ResponseBuilder,
};
pub use meta::*;
#[cfg(not(target_arch = "wasm32"))]
pub use observability::{ObservabilityBus, ObservabilityContext};
pub use block::{AsyncFuncBlock, FuncBlock};
pub use router::Router;
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::{RuntimeHandle, Wafer};
#[cfg(all(feature = "wasm", not(target_arch = "wasm32")))]
pub use runtime::{
    parse_unversioned_block, parse_versioned_block, RemoteBlockRef, UnversionedRemoteBlockRef,
};
pub use types::{
    Action, BlockResult, BlockRuntime, InstanceMode, LifecycleEvent, LifecycleType, Message,
    RequestAction, Response, Result_, WaferError,
};

#[cfg(feature = "wasm")]
pub use wasm::WASMBlock;
pub use wasm::capabilities::BlockCapabilities;
