//! WAFER — WebAssembly Architecture for Flow Execution & Routing
//!
//! A message-processing runtime that executes flows of blocks.
//! Each block receives a message, processes it, and returns a result
//! that determines the next step in the flow.

#![warn(missing_docs)]

pub mod asset_loader;
pub mod block;
/// Shared identifiers (error codes, service names) used across host/guest boundaries.
pub mod common;
pub mod compat;
pub mod config;
pub mod context;
#[cfg(not(target_arch = "wasm32"))]
pub mod discovery;
pub mod error;
pub mod executor;
pub mod helpers;
/// Re-exports of the canonical metadata constants defined in `wafer-block`.
pub mod meta;
/// Observability hooks: pluggable callbacks fired on flow/block lifecycle events.
pub mod observability;
pub mod platform;
mod registry_loader;
pub mod router;
/// Top-level runtime: the `Wafer` instance, block slots, config sources and validation.
pub mod runtime;
/// SSRF defenses and other security helpers shared by host- and native-side fetchers.
pub mod security;
pub mod snapshot;
/// Re-exports of the core runtime value types (`Message`, `MetaEntry`, `WaferError`, …) from `wafer-block`.
pub mod types;
/// Executor for `WaferFlow` definitions — runs a sequence of blocks against an input message.
pub mod waferflow;
/// WASM block loader and host capabilities (gated by the `wasmi` feature on native targets).
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
pub use runtime::{
    config_source::{ConfigError, ConfigSource, EnvBlockConfig, StaticConfigSource},
    slot::{BlockSlot, InitError, InitializedState},
    BrokenBlock, ValidationReport, Wafer,
};
#[cfg(all(feature = "wasm", not(target_arch = "wasm32")))]
pub use runtime::{parse_unversioned_block, parse_versioned_block, RemoteBlockRef, ABI_VERSION};
mod builder;
pub use builder::WaferBuilder;
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
