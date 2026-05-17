//! Local filesystem storage service implementation for WAFER.
//!
//! Provides `LocalStorageService` implementing `StorageService`.
//! The trait is defined in `wafer_core::interfaces::storage`.
//!
//! Use `wafer_core::service_blocks::storage::register_with()` to register.

#![warn(missing_docs)]

/// Filesystem-backed `StorageService` implementation and its sandboxed-path helpers.
pub mod service;
