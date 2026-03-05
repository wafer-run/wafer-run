//! wafer-core — Shared reusable WAFER blocks and flow templates.
//!
//! This crate provides all WAFER blocks and the client API layer.
//! wafer-run is a pure runtime engine; wafer-core owns the block library.

pub mod blocks;
pub mod flows;
pub mod clients;

/// Register all wafer-core blocks with a Wafer runtime.
pub fn register_all(w: &mut wafer_run::Wafer) {
    use std::sync::Arc;

    macro_rules! register_factory {
        ($w:expr, $name:expr, $factory:expr) => {
            if let Err(e) = $w.registry().register($name, Arc::new($factory)) {
                tracing::warn!(block = $name, error = %e, "failed to register block factory");
            }
        };
    }

    blocks::auth::register(w);
    register_factory!(w, "@wafer/config", blocks::config::factory::ConfigBlockFactory);
    blocks::cors::register(w);
    #[cfg(feature = "crypto")]
    register_factory!(w, "@wafer/crypto", blocks::crypto::factory::CryptoBlockFactory);
    #[cfg(feature = "sqlite")]
    register_factory!(w, "@wafer/database", blocks::database::factory::DatabaseBlockFactory);
    #[cfg(all(not(feature = "sqlite"), feature = "postgres"))]
    register_factory!(w, "@wafer/database", blocks::database::factory::DatabaseBlockFactory);
    #[cfg(feature = "http")]
    blocks::http::register(w);
    blocks::iam::register(w);
    blocks::inspector::register(w);
    register_factory!(w, "@wafer/logger", blocks::logger::factory::LoggerBlockFactory);
    blocks::monitoring::register(w);
    #[cfg(feature = "network")]
    register_factory!(w, "@wafer/network", blocks::network::factory::NetworkBlockFactory);
    blocks::rate_limit::register(w);
    blocks::readonly_guard::register(w);
    blocks::router::register(w);
    blocks::security_headers::register(w);
    #[cfg(feature = "storage-local")]
    register_factory!(w, "@wafer/storage", blocks::storage::factory::StorageBlockFactory);
    #[cfg(all(not(feature = "storage-local"), feature = "storage-s3"))]
    register_factory!(w, "@wafer/storage", blocks::storage::factory::StorageBlockFactory);
    blocks::web::register(w);
}
