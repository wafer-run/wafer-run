//! wafer-core — Shared reusable WAFER blocks and chain templates.
//!
//! This crate provides all WAFER blocks (infrastructure + application)
//! and the client API layer. wafer-run is a pure runtime engine;
//! wafer-core owns the block library.

pub mod blocks;
#[cfg(feature = "http")]
pub mod bridge;
pub mod chains;
pub mod clients;

/// Register all wafer-core blocks with a Wafer runtime.
pub fn register_all(w: &mut wafer_run::Wafer) {
    use std::sync::Arc;

    // Infrastructure block factories
    #[cfg(feature = "sqlite")]
    w.registry().register("wafer/database", Arc::new(blocks::database::factory::DatabaseBlockFactory)).ok();
    #[cfg(all(not(feature = "sqlite"), feature = "postgres"))]
    w.registry().register("wafer/database", Arc::new(blocks::database::factory::DatabaseBlockFactory)).ok();
    #[cfg(feature = "storage-local")]
    w.registry().register("wafer/storage", Arc::new(blocks::storage::factory::StorageBlockFactory)).ok();
    #[cfg(all(not(feature = "storage-local"), feature = "storage-s3"))]
    w.registry().register("wafer/storage", Arc::new(blocks::storage::factory::StorageBlockFactory)).ok();
    #[cfg(feature = "crypto")]
    w.registry().register("wafer/crypto", Arc::new(blocks::crypto::factory::CryptoBlockFactory)).ok();
    #[cfg(feature = "network")]
    w.registry().register("wafer/network", Arc::new(blocks::network::factory::NetworkBlockFactory)).ok();
    w.registry().register("wafer/logger", Arc::new(blocks::logger::factory::LoggerBlockFactory)).ok();
    w.registry().register("wafer/config", Arc::new(blocks::config::factory::ConfigBlockFactory)).ok();

    // Application blocks
    blocks::http_router::register(w);
    blocks::security_headers::register(w);
    blocks::cors::register(w);
    blocks::rate_limit::register(w);
    blocks::readonly_guard::register(w);
    blocks::monitoring::register(w);
    blocks::auth::register(w);
    blocks::iam::register(w);
    blocks::web::register(w);
}
