//! WAFER — WebAssembly Architecture for Flow Execution & Routing
//!
//! A message-processing runtime that executes chains of blocks.
//! Each block receives a message, processes it, and returns a result
//! that determines the next step in the chain.

pub mod block;
pub mod bridge;
pub mod common;
pub mod config;
pub mod context;
pub mod executor;
pub mod helpers;
pub mod manifest;
pub mod meta;
pub mod observability;
pub mod registry;
pub mod router;
pub mod runtime;
pub mod schema;
pub mod security;
pub mod types;
pub mod wasm;

// Re-exports for convenience
pub use block::{AdminUIInfo, Block, BlockInfo};
pub use config::{
    Chain, ChainConfig, ChainConfigDef, ChainDef, ChainInfo, HTTPRoute, HTTPRouteDef, Node,
    NodeDef,
};
pub use context::{Context, RuntimeContext};
pub use executor::{extract_path_vars, match_path, matches_pattern};
pub use helpers::{
    err_bad_request, err_conflict, err_forbidden, err_internal, err_not_found, err_unauthorized,
    err_validation, error, expand_env_vars, json_respond, new_response, respond, sha256_hex,
    ResponseBuilder,
};
pub use meta::*;
pub use observability::{ObservabilityBus, ObservabilityContext};
pub use registry::{BlockFactory, FuncBlock, Registry};
pub use router::Router;
pub use runtime::Wafer;
#[cfg(feature = "wasm")]
pub use runtime::{
    parse_unversioned_block, parse_versioned_block, RemoteBlockRef, UnversionedRemoteBlockRef,
};
pub use types::{
    Action, InstanceMode, LifecycleEvent, LifecycleType, Message, RequestAction, Response,
    Result_, WaferError,
};

#[cfg(feature = "wasm")]
pub use wasm::WASMBlock;
pub use wasm::capabilities::BlockCapabilities;
