//! Unified service block implementations.
//!
//! Each block wraps an `Arc<dyn XService>` and delegates to the shared handler
//! in `wafer_core::interfaces`. Platform-specific code only provides the service
//! implementation; the block struct, `info()`, and message routing are shared.

pub mod auth;
pub mod config;
pub mod crypto;
pub mod database;
pub mod llm;
pub mod logger;
pub mod network;
pub mod storage;
pub mod vector;
