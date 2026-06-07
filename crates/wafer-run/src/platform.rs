//! Platform-specific type aliases and utilities.
//!
//! This module provides cross-platform abstractions for types that differ
//! between native and wasm32 targets. Using `web-time` for Instant (zero-cost
//! on native, Performance.now() on wasm32) and conditional Send/Sync bounds.

use std::{future::Future, pin::Pin};

/// Cross-platform monotonic [`Instant`].
///
/// Re-exported from [`web_time`], which is a zero-cost re-export of
/// [`std::time::Instant`] on native targets and a `Performance.now()`-backed
/// monotonic clock on wasm32 (browser) targets. The runtime is compiled to
/// wasm32 only for the in-browser host (solobase-web), where `Performance.now()`
/// is available, so deadline checks and elapsed-time measurements compute real
/// durations on every supported target.
///
/// A previous wasm32 build of this module shipped a unit-struct stub whose
/// `now()`/`Add` were no-ops; because every stub value compared `Equal`,
/// `Instant::now() >= deadline` was always true and any flow with a configured
/// timeout was reported cancelled/`DEADLINE_EXCEEDED` on its first check. Using
/// the real `web_time::Instant` on all targets fixes that.
pub use web_time::Instant;

// ---------------------------------------------------------------------------
// Boxed future type aliases (Send on native, !Send on wasm32)
// ---------------------------------------------------------------------------

/// Boxed future returned across runtime async boundaries (native — requires `Send`).
#[cfg(not(target_arch = "wasm32"))]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Boxed future returned across runtime async boundaries (wasm32 — single-threaded).
#[cfg(target_arch = "wasm32")]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

// ---------------------------------------------------------------------------
// Closure type aliases for runtime internals
// ---------------------------------------------------------------------------

/// RegistrarFn — function that registers a block or flow with config.
#[cfg(not(target_arch = "wasm32"))]
pub type RegistrarFn = Box<dyn Fn(&mut crate::runtime::Wafer, serde_json::Value) + Send + Sync>;
/// RegistrarFn — function that registers a block or flow with config
/// (wasm32 — single-threaded, no `Send + Sync` bound).
#[cfg(target_arch = "wasm32")]
pub type RegistrarFn = Box<dyn Fn(&mut crate::runtime::Wafer, serde_json::Value)>;

/// ConfigExpanderFn — function that splits a composite config into individual block configs.
#[cfg(not(target_arch = "wasm32"))]
pub type ConfigExpanderFn =
    Box<dyn Fn(serde_json::Value) -> Vec<(String, serde_json::Value)> + Send + Sync>;
/// ConfigExpanderFn — function that splits a composite config into individual block
/// configs (wasm32 — single-threaded, no `Send + Sync` bound).
#[cfg(target_arch = "wasm32")]
pub type ConfigExpanderFn = Box<dyn Fn(serde_json::Value) -> Vec<(String, serde_json::Value)>>;
