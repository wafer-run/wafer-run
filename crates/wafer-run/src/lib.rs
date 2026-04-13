//! WAFER — WebAssembly Architecture for Flow Execution & Routing
//!
//! A message-processing runtime that executes flows of blocks.
//! Each block receives a message, processes it, and returns a result
//! that determines the next step in the flow.

pub mod block;
pub mod common;
pub mod compat;
pub mod config;
pub mod context;
pub mod discovery;
pub mod executor;
pub mod helpers;
pub mod manifest;
pub mod meta;
pub mod observability;
pub mod platform;
pub mod router;
pub mod runtime;
pub mod schema;
pub mod security;
pub mod types;
pub mod waferflow;
pub mod wasm;

// Re-export the WRAP access control module from wafer-block
pub use wafer_block::wrap;

// Re-exports for convenience
pub use block::{AsyncFuncBlock, FuncBlock};
pub use block::{Block, BlockCategory, BlockInfo, BlockRuntime, UiRoute};
pub use compat::{MaybeSend, MaybeSync};
pub use config::{BlockConfig, DispatchTarget};
pub use context::{Context, RuntimeContext};
pub use executor::{extract_path_vars, match_path, matches_pattern};
#[cfg(not(target_arch = "wasm32"))]
pub use helpers::expand_env_vars;
pub use helpers::{
    err_bad_request, err_conflict, err_forbidden, err_internal, err_not_found, err_unauthorized,
    err_validation, error, json_respond, new_response, respond, respond_empty, respond_json,
    sha256_hex, ResponseBuilder,
};
pub use meta::*;
pub use observability::{ObservabilityBus, ObservabilityContext};
pub use router::Router;
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::RuntimeHandle;
pub use runtime::Wafer;
#[cfg(all(feature = "wasm", not(target_arch = "wasm32")))]
pub use runtime::{parse_unversioned_block, parse_versioned_block, RemoteBlockRef, ABI_VERSION};
pub use types::{
    Action, AuthLevel, BlockResult, HttpMethod, InstanceMode, LifecycleEvent, LifecycleType,
    Message, RequestAction, ResourceGrant, ResourceType, Response, Result_, WaferError,
};

pub use wasm::capabilities::BlockCapabilities;
#[cfg(feature = "wasmi")]
pub use wasm::WasmiBlock;
