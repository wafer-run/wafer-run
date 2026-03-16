//! Platform-specific type aliases and utilities.
//!
//! This module provides cross-platform abstractions for types that differ
//! between native and wasm32 targets. Using `web-time` for Instant (zero-cost
//! on native, Performance.now() on wasm32) and conditional Send/Sync bounds.

use std::future::Future;
use std::pin::Pin;

/// Cross-platform Instant. On native, this is `std::time::Instant`.
/// On wasm32, this uses `Performance.now()` via the `web-time` crate.
pub use web_time::Instant;

// ---------------------------------------------------------------------------
// Boxed future type aliases (Send on native, !Send on wasm32)
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[cfg(target_arch = "wasm32")]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

// ---------------------------------------------------------------------------
// Closure type aliases for runtime internals
// ---------------------------------------------------------------------------

/// RegistrarFn — function that registers a block or flow with config.
#[cfg(not(target_arch = "wasm32"))]
pub type RegistrarFn = Box<dyn Fn(&mut crate::runtime::Wafer, serde_json::Value) + Send + Sync>;
#[cfg(target_arch = "wasm32")]
pub type RegistrarFn = Box<dyn Fn(&mut crate::runtime::Wafer, serde_json::Value)>;

/// ConfigExpanderFn — function that splits a composite config into individual block configs.
#[cfg(not(target_arch = "wasm32"))]
pub type ConfigExpanderFn = Box<dyn Fn(serde_json::Value) -> Vec<(String, serde_json::Value)> + Send + Sync>;
#[cfg(target_arch = "wasm32")]
pub type ConfigExpanderFn = Box<dyn Fn(serde_json::Value) -> Vec<(String, serde_json::Value)>>;
