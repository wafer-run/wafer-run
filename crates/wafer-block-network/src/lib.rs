//! Network service implementations for WAFER.
//!
//! Provides `HttpNetworkService` (async reqwest with SSRF protection) for native use.
//! The `NetworkService` trait is re-exported from `wafer_core::interfaces::network`.
//!
//! Use `wafer_core::service_blocks::network::register_with()` to register.

pub mod service;
