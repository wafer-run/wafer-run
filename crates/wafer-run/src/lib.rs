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
/// Block manifest schema — versioned JSON descriptor of a block's interface, config and grants.
pub mod manifest;
/// Re-exports of the canonical metadata constants defined in `wafer-block`.
pub mod meta;
/// Observability hooks: pluggable callbacks fired on flow/block lifecycle events.
pub mod observability;
pub mod platform;
mod registry_loader;
pub mod router;
/// Top-level runtime: the `Wafer` instance, block slots, config sources and validation.
pub mod runtime;
/// JSON Schema types used by block manifests to describe message/config shape.
pub mod schema;
/// SSRF defenses and other security helpers shared by host- and native-side fetchers.
pub mod security;
pub mod snapshot;
#[cfg(not(target_arch = "wasm32"))]
pub mod static_registration;
/// Re-exports of the core runtime value types (`Message`, `MetaEntry`, `WaferError`, …) from `wafer-block`.
pub mod types;
/// Executor for `WaferFlow` definitions — runs a sequence of blocks against an input message.
pub mod waferflow;
/// WASM block loader and host capabilities (gated by the `wasmi` feature on native targets).
pub mod wasm;

/// Register a block at link time via `linkme`.
///
/// This macro inserts a [`StaticBlockRegistration`] entry into the
/// [`STATIC_BLOCK_REGISTRATIONS`] distributed slice. The linker includes the
/// entry in the final binary regardless of whether any code-level symbol from
/// the crate is otherwise referenced — the key property that makes this safe
/// for standalone `wafer-block-*` crates (unlike `inventory::submit!`, which
/// was silently dropped by the linker when no other symbol from the crate was
/// reachable).
///
/// ## Usage
///
/// ```rust,ignore
/// wafer_run::register_static_block!("my-org/my-block", MyBlockType);
/// ```
///
/// The type `$ty` must implement [`Block`] and have a `fn new() -> Self`
/// constructor.
///
/// ## Escape hatch
///
/// If the block type requires a non-`new()` factory (e.g. it needs runtime
/// config to construct), you can use the `linkme` attribute directly against
/// `$crate::STATIC_BLOCK_REGISTRATIONS` with `#[linkme(crate = $crate::linkme)]`.
/// That is an internal API — prefer wrapping the config in the block's
/// `Block::setup` lifecycle instead.
///
/// This macro is a no-op on `wasm32` targets.
#[cfg(not(target_arch = "wasm32"))]
#[macro_export]
macro_rules! register_static_block {
    ($name:expr, $ty:ty) => {
        const _: () = {
            #[$crate::linkme::distributed_slice($crate::STATIC_BLOCK_REGISTRATIONS)]
            #[linkme(crate = $crate::linkme)]
            static REGISTRATION: $crate::StaticBlockRegistration =
                $crate::StaticBlockRegistration {
                    name: $name,
                    factory: || {
                        ::std::sync::Arc::new(<$ty>::new()) as ::std::sync::Arc<dyn $crate::Block>
                    },
                };
        };
    };
}

/// No-op on `wasm32` — linkme is not supported on WASM targets.
#[cfg(target_arch = "wasm32")]
#[macro_export]
macro_rules! register_static_block {
    ($name:expr, $ty:ty) => {};
}

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
// Re-export linkme so `register_static_block!` expansions in consumer crates
// can refer to `$crate::linkme` without adding linkme to their own Cargo.toml.
pub use linkme;
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
#[cfg(not(target_arch = "wasm32"))]
pub use static_registration::{StaticBlockRegistration, STATIC_BLOCK_REGISTRATIONS};
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
