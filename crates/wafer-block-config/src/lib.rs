//! Configuration service implementations for WAFER.
//!
//! Provides `EnvConfigService` (env vars + overrides) for native use.
//! The `ConfigService` trait is re-exported from `wafer_core::interfaces::config`.
//!
//! Use `wafer_core::service_blocks::config::register_with()` to register.

pub mod service;
#[cfg(feature = "toml")]
pub mod toml;
