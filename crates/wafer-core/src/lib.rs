//! wafer-core — Shared interfaces, clients, and utilities for WAFER blocks.
//!
//! This crate provides:
//! - `interfaces/` — DatabaseService, StorageService traits + shared handlers
//! - `clients/` — RPC wrappers for calling blocks (database, storage, crypto, etc.)
//! - `mime` — MIME type detection utility

#![warn(missing_docs)]

/// RPC client wrappers that let a block call into another block's service interface.
pub mod clients;
/// Discovery helpers that resolve aliases such as `@wafer-run/wafer-run/database` to the
/// concrete block name registered in the current runtime.
pub mod discovery;
/// Service trait definitions plus the host-side message handlers that bridge them to the
/// WAFER message ABI used by both native and WASM blocks.
pub mod interfaces;
/// MIME-type detection helper used by the storage and image interfaces.
pub mod mime;
/// SSRF defenses (blocked-IP / blocked-URL predicates) shared by host- and
/// native-side fetchers. Lives here so leaf blocks can use it without
/// depending on the `wafer-run` runtime crate.
pub mod security;
/// Adapters that wrap a plain `Arc<dyn …Service>` so it can be registered with the runtime as
/// a native WAFER block implementing the matching interface.
pub mod service_blocks;

#[cfg(test)]
pub mod test_support;
