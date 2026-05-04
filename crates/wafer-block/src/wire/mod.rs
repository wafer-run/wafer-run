//! Shared wire-format types. Consumed by `wafer_core` (host handlers + native
//! clients) and `wafer_sdk` (WASM skill clients). Encoded via `wafer_block::codec`
//! (currently MessagePack).
//!
//! Each service interface gets its own submodule. Types are pure DTOs —
//! `#[derive(Serialize, Deserialize, Debug, Clone)]` only, no behavior.

pub mod network;
pub mod storage;
// Subsequent services added in their own tasks: database, vector, etc.
