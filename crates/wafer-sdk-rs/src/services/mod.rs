//! Service clients using WIT-generated imports.
//!
//! Each module provides typed functions that call directly into the host via
//! the WebAssembly Component Model — no manual serialization needed.

pub mod config;
pub mod crypto;
pub mod database;
pub mod logger;
pub mod network;
pub mod storage;
