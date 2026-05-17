#![warn(missing_docs)]
//! Network service implementations for WAFER.
//!
//! Provides `HttpNetworkService` (async reqwest with SSRF protection) for native use.
//! The `NetworkService` trait is re-exported from `wafer_core::interfaces::network`.
//!
//! Use `wafer_core::service_blocks::network::register_with()` to register.

/// Concrete [`NetworkService`](wafer_core::interfaces::network::service::NetworkService)
/// implementation backed by `reqwest`, plus the SSRF-aware DNS resolver and the
/// response-size cap helpers that defend outbound HTTP from internal-network and
/// unbounded-body attacks.
pub mod service;
