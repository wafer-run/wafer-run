//! wafer-core — Shared interfaces, clients, and utilities for WAFER blocks.
//!
//! This crate provides:
//! - `interfaces/` — DatabaseService, StorageService traits + shared handlers
//! - `clients/` — RPC wrappers for calling blocks (database, storage, crypto, etc.)
//! - `mime` — MIME type detection utility

pub mod clients;
pub mod discovery;
pub mod interfaces;
pub mod mime;
pub mod service_blocks;

#[cfg(test)]
pub mod test_support;
