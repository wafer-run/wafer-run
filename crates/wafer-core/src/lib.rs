//! wafer-core — Shared interfaces, clients, and utilities for WAFER blocks.
//!
//! This crate provides:
//! - `interfaces/` — DatabaseService, StorageService traits + shared handlers
//! - `clients/` — RPC wrappers for calling blocks (database, storage, crypto, etc.)
//! - `mime` — MIME type detection utility

pub mod clients;
pub mod interfaces;
pub mod mime;
