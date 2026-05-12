//! Shared wire-format types. Consumed by `wafer_core` (host handlers + native
//! clients) and `wafer_sdk` (WASM skill clients). Encoded via `wafer_block::codec`
//! (currently MessagePack).
//!
//! Each service interface gets its own submodule. Types are pure DTOs —
//! `#[derive(Serialize, Deserialize, Debug, Clone)]` only, no behavior.

pub mod auth;
pub mod config;
pub mod crypto;
pub mod database;
pub mod image;
pub mod llm;
pub mod logger;
pub mod network;
pub mod storage;
pub mod vector;
