//! Service clients that call infrastructure blocks via `call-block`.
//!
//! Each module provides typed functions that build a message, call the
//! corresponding infrastructure block (e.g. `wafer/database`), and parse
//! the result back into ergonomic Rust types.

pub mod config;
pub mod crypto;
pub mod database;
pub mod logger;
pub mod network;
pub mod storage;
