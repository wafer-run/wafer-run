//! Platform-specific type aliases and utilities.
//!
//! This module provides cross-platform abstractions for types that differ
//! between native and wasm32 targets. Using `web-time` for Instant (zero-cost
//! on native, Performance.now() on wasm32) and conditional Send/Sync bounds.

use std::{future::Future, pin::Pin};

/// Cross-platform Instant. On native, this is `std::time::Instant` (via web-time,
/// which is a zero-cost wrapper). On wasm32 Component targets, a minimal stub
/// is provided since there's no system clock available.
#[cfg(not(target_arch = "wasm32"))]
pub use web_time::Instant;

/// Cross-platform Instant. On native this re-exports `web_time::Instant`; on
/// wasm32 Component targets there is no system clock available, so this is a
/// minimal zero-sized stub whose duration methods all return
/// [`Duration::ZERO`](std::time::Duration::ZERO).
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant;

#[cfg(target_arch = "wasm32")]
impl Instant {
    /// Returns the current instant. On wasm32 this is a no-op stub value.
    pub fn now() -> Self {
        Self
    }
    /// Returns the time elapsed since this instant. Always `Duration::ZERO` on wasm32.
    pub fn elapsed(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
    /// Returns the duration since `earlier`, or `None` on overflow. Always
    /// `Some(Duration::ZERO)` on wasm32.
    pub fn checked_duration_since(&self, _earlier: Instant) -> Option<std::time::Duration> {
        Some(std::time::Duration::ZERO)
    }
    /// Returns the duration since `earlier`. Always `Duration::ZERO` on wasm32.
    pub fn duration_since(&self, _earlier: Instant) -> std::time::Duration {
        std::time::Duration::ZERO
    }
}

#[cfg(target_arch = "wasm32")]
impl std::ops::Add<std::time::Duration> for Instant {
    type Output = Instant;
    fn add(self, _rhs: std::time::Duration) -> Instant {
        self
    }
}

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
