//! WAFER — WebAssembly Architecture for Flow Execution & Routing
//!
//! A message-processing runtime that executes flows of blocks.
//! Each block receives a message, processes it, and returns a result
//! that determines the next step in the flow.

#![warn(missing_docs)]

pub mod asset_loader;
pub mod context;
#[cfg(not(target_arch = "wasm32"))]
pub mod discovery;
/// Shared helpers for embedder bindings (`wafer-ffi`, `wafer-run-node`): the
/// `{"action": ...}` JSON wire format and registration-from-path policy.
pub mod embed;
/// Observability hooks: pluggable callbacks fired on flow/block lifecycle events.
pub mod observability;
pub mod platform;
mod registry_loader;
/// Top-level runtime: the `Wafer` instance, block slots, config sources and validation.
pub mod runtime;
pub mod snapshot;
/// Executor for `WaferFlow` definitions — runs a sequence of blocks against an input message.
pub mod waferflow;
/// WASM block loader and host capabilities (gated by the `wasmi` feature on native targets).
pub mod wasm;

// Re-exports of the canonical shared types defined in `wafer-block`. One
// canonical `wafer_run::X` path per item — the former 1-2 line mirror
// modules (`types`, `meta`, `block`, `error`, ...) are gone.
pub use asset_loader::{AssetLoadError, AssetLoadStatus, LoadAssetCallback, NoopAssetLoader};
pub use context::{Context, RuntimeContext};
pub use observability::{ObservabilityBus, ObservabilityContext};
#[cfg(all(feature = "wasm", not(target_arch = "wasm32")))]
pub use runtime::remote::{
    parse_unversioned_block, parse_versioned_block, RemoteBlockRef, ABI_VERSION,
};
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::RuntimeHandle;
pub use runtime::{
    config_source::{ConfigError, ConfigSource, EnvBlockConfig, StaticConfigSource},
    slot::{BlockSlot, InitError, InitializedState},
    wasm_state::{FuelLimit, ResourceLimits, DEFAULT_FUEL, DEFAULT_MAX_WASM_MEMORY_PAGES},
    BrokenBlock, ValidationReport, Wafer,
};
#[cfg(not(target_arch = "wasm32"))]
pub use wafer_block::expand_env_vars;
pub use wafer_block::{
    block::Block,
    common,
    compat::{MaybeSend, MaybeSync},
    config::{BlockConfig, DispatchTarget},
    error::{
        AliasError, BlockReferenceError, BlockReferenceSource, GrantValidationError, RuntimeError,
    },
    executor::{extract_path_vars, match_path, matches_pattern},
    // Canonical hash/hex helpers (from wafer_block::hash) — re-exported at the
    // root so consumers (e.g. solobase-core) use this single implementation
    // instead of carrying copies. (#163)
    hex_encode,
    meta::*,
    registry::BlockRegistry,
    sha256,
    sha256_hex,
    streams,
    streams::{
        input::InputStream,
        output::{OutputSink, OutputStream, TerminalNotResponse},
    },
    types::{
        AuthLevel, BlockCategory, BlockEndpoint, BlockInfo, BlockRuntime, CollectionSchema,
        ConfigVar, FieldSchema, HttpMethod, IndexSchema, InputType, MetaGet, MetaSet,
        RequestAction, ResourceGrant, ResourceType,
    },
    wrap,
    ErrorCode,
    InstanceMode,
    LifecycleEvent,
    LifecycleType,
    Message,
    MetaEntry,
    WaferError,
};
mod builder;
pub use builder::WaferBuilder;
pub use wasm::capabilities::BlockCapabilities;
#[cfg(feature = "wasmi")]
pub use wasm::WasmiBlock;
