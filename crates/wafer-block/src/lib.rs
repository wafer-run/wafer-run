//! Shared types, traits, and macros for WAFER block authors.
//!
//! This crate provides the core types used by both the WAFER runtime (`wafer-run`)
//! and the WASM guest SDK (`wafer-sdk`). Block authors depend on this crate for
//! type definitions, and optionally use the `#[wafer_block]` proc macro for
//! reduced boilerplate.

#![warn(missing_docs)]

pub mod abi;
pub mod codec;
pub mod core_types;
pub mod error;
pub mod http_codec;
pub mod meta;
pub mod types;
pub mod wire;

// Re-export the proc macro.
// Re-export core types at crate root.
pub use core_types::{
    Attachment, ErrorCode, InstanceMode, LifecycleEvent, LifecycleType, Message, MetaEntry,
    WaferError,
};
pub use meta::*;
// Re-export runtime-specific types (needed by block authors for BlockInfo etc.)
pub use types::{
    ActionSpec, AuthLevel, BlockCategory, BlockEndpoint, BlockInfo, BlockInfoError, BlockRuntime,
    CollectionSchema, ConfigVar, ExternalAsset, FieldSchema, HttpMethod, IndexSchema, InputType,
    InterfaceSpec, MetaGet, MetaSet, RequestAction, ResourceGrant, ResourceType, SkillTool,
    WAFER_RUN_SHARED_PREFIX,
};
pub use wafer_block_macro::{wafer_async_trait, wafer_block};

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
pub mod db;
pub mod executor;
pub mod hash;
pub mod interfaces;
pub mod introspection;
pub mod lockfile;
pub mod registry;
pub mod response;
pub mod runtime;
pub mod spawn;
#[cfg(not(target_arch = "wasm32"))]
pub mod static_registration;
pub mod stream;
pub mod streams;
pub mod validation;
pub mod wrap;

pub use block::Block;
pub use capabilities::{Allowlist, BlockCapabilities};
pub use common::{ServiceName, ServiceOp};
pub use compat::{MaybeSend, MaybeSync};
pub use config::{BlockConfig, DispatchTarget};
pub use context::Context;
pub use error::{BlockConfigRef, RuntimeError};
pub use executor::{extract_path_vars, match_path, matches_pattern};
#[cfg(not(target_arch = "wasm32"))]
pub use hash::expand_env_vars;
pub use hash::{hex_encode, sha256, sha256_hex};
pub use introspection::FlowIntrospection;
// Re-export linkme so `register_static_block!` expansions in consumer crates
// can refer to `$crate::linkme` without adding linkme to their own Cargo.toml.
#[cfg(not(target_arch = "wasm32"))]
pub use linkme;
pub use registry::BlockRegistry;
pub use response::{
    err_bad_request, err_conflict, err_forbidden, err_internal, err_internal_no_cause,
    err_not_found, err_unauthorized, ok_empty, ok_json, ResponseBuilder,
};
pub use runtime::Runtime;
pub use spawn::spawn_producer;
#[cfg(not(target_arch = "wasm32"))]
pub use static_registration::{StaticBlockRegistration, STATIC_BLOCK_REGISTRATIONS};
pub use stream::StreamEvent;
pub use streams::{
    input::InputStream,
    output::{
        BufferedResponse, OutputSink, OutputStream, SinkClosed, SinkSendError, TerminalNotResponse,
    },
};
pub use validation::{unknown_flow_config_keys, BrokenBlock, ValidationReport};

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
/// wafer_block::register_static_block!("my-org/my-block", MyBlockType);
/// ```
///
/// The type `$ty` must implement [`Block`] and have a `fn new() -> Self`
/// constructor.
///
/// ## Escape hatch
///
/// If the block type requires a non-`new()` factory (e.g. it needs runtime
/// config to construct), you can use the `linkme` attribute directly against
/// `wafer_block::STATIC_BLOCK_REGISTRATIONS` with `#[linkme(crate = $crate::linkme)]`.
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

/// Force-link a set of block crates so each crate's
/// `register_static_block!` entry survives the linker.
///
/// Takes a comma-separated list of **crate names** (underscored, e.g.
/// `wafer_block_cors`). Each name expands to `use ::<name> as _;`, which
/// pulls the crate's object file into the binary so its
/// `linkme::distributed_slice` registration entries are retained. The
/// consumer's `Cargo.toml` must declare each named crate as a dependency —
/// the macro generates use-statements, not Cargo entries.
///
/// The consumer supplies the block list; this macro is just the linking
/// machinery and works for any block crate (first- or third-party), not a
/// hardcoded battery roster.
///
/// # Example
///
/// ```ignore
/// wafer_block::use_static_blocks!(wafer_block_cors, wafer_block_security_headers);
/// // expands to:
/// // use ::wafer_block_cors as _;
/// // use ::wafer_block_security_headers as _;
/// ```
#[macro_export]
macro_rules! use_static_blocks {
    ($($krate:ident),* $(,)?) => {
        $( use ::$krate as _; )*
    };
}
