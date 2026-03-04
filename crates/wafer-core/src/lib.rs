//! wafer-core — Shared reusable WAFER blocks and flow templates.
//!
//! This crate provides all WAFER blocks (infrastructure + application)
//! and the client API layer. wafer-run is a pure runtime engine;
//! wafer-core owns the block library.

pub mod blocks;
#[cfg(feature = "http")]
pub mod bridge;
pub mod flows;
pub mod clients;

/// Register all wafer-core blocks with a Wafer runtime.
pub fn register_all(w: &mut wafer_run::Wafer) {
    use std::sync::Arc;

    // Infrastructure block factories — log warnings on registration failure
    macro_rules! register_factory {
        ($w:expr, $name:expr, $factory:expr) => {
            if let Err(e) = $w.registry().register($name, Arc::new($factory)) {
                tracing::warn!(block = $name, error = %e, "failed to register block factory");
            }
        };
    }

    #[cfg(feature = "sqlite")]
    register_factory!(w, "wafer/database", blocks::database::factory::DatabaseBlockFactory);
    #[cfg(all(not(feature = "sqlite"), feature = "postgres"))]
    register_factory!(w, "wafer/database", blocks::database::factory::DatabaseBlockFactory);
    #[cfg(feature = "storage-local")]
    register_factory!(w, "wafer/storage", blocks::storage::factory::StorageBlockFactory);
    #[cfg(all(not(feature = "storage-local"), feature = "storage-s3"))]
    register_factory!(w, "wafer/storage", blocks::storage::factory::StorageBlockFactory);
    #[cfg(feature = "crypto")]
    register_factory!(w, "wafer/crypto", blocks::crypto::factory::CryptoBlockFactory);
    #[cfg(feature = "network")]
    register_factory!(w, "wafer/network", blocks::network::factory::NetworkBlockFactory);
    register_factory!(w, "wafer/logger", blocks::logger::factory::LoggerBlockFactory);
    register_factory!(w, "wafer/config", blocks::config::factory::ConfigBlockFactory);

    // Application blocks
    blocks::http_router::register(w);
    blocks::security_headers::register(w);
    blocks::cors::register(w);
    blocks::rate_limit::register(w);
    blocks::readonly_guard::register(w);
    blocks::monitoring::register(w);
    blocks::inspector::register(w);
    blocks::auth::register(w);
    blocks::iam::register(w);
    blocks::web::register(w);
}
