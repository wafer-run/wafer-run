//! WAFER — WebAssembly Architecture for Flow Execution & Routing
//!
//! A message-processing runtime that executes flows of blocks.
//! Each block receives a message, processes it, and returns a result
//! that determines the next step in the flow.

pub mod asset_loader;
pub mod block;
pub mod common;
pub mod compat;
pub mod config;
pub mod context;
#[cfg(not(target_arch = "wasm32"))]
pub mod discovery;
pub mod error;
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
// Re-exports for convenience
pub use asset_loader::{AssetLoadError, AssetLoadStatus, LoadAssetCallback, NoopAssetLoader};
pub use block::{Block, BlockCategory, BlockInfo, BlockRuntime, UiRoute};
pub use compat::{MaybeSend, MaybeSync};
pub use config::{BlockConfig, DispatchTarget};
pub use context::{Context, RuntimeContext};
pub use error::RuntimeError;
pub use executor::{extract_path_vars, match_path, matches_pattern};
#[cfg(not(target_arch = "wasm32"))]
pub use helpers::expand_env_vars;
pub use helpers::sha256_hex;
pub use meta::*;
pub use observability::{ObservabilityBus, ObservabilityContext};
pub use router::Router;
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::RuntimeHandle;
pub use runtime::Wafer;
#[cfg(all(feature = "wasm", not(target_arch = "wasm32")))]
pub use runtime::{parse_unversioned_block, parse_versioned_block, RemoteBlockRef, ABI_VERSION};
pub use types::{
    AuthLevel, ErrorCode, HttpMethod, InstanceMode, LifecycleEvent, LifecycleType, Message,
    MetaEntry, RequestAction, ResourceGrant, ResourceType, WaferError,
};
pub use wafer_block::{
    registry::BlockRegistry,
    streams,
    streams::{
        input::InputStream,
        output::{OutputSink, OutputStream, TerminalNotResponse},
    },
    wrap,
};
pub use wasm::capabilities::BlockCapabilities;
#[cfg(feature = "wasmi")]
pub use wasm::WasmiBlock;
