//! wafer-core — Shared reusable WAFER blocks and chain templates.
//!
//! This crate provides infrastructure blocks (security headers, CORS,
//! rate limiting, auth, etc.) and chain templates that can be used by
//! any WAFER application.

pub mod blocks;
pub mod chains;

/// Register all wafer-core blocks with a Wafer runtime.
pub fn register_all(w: &mut wafer_run::Wafer) {
    blocks::security_headers::register(w);
    blocks::cors::register(w);
    blocks::rate_limit::register(w);
    blocks::readonly_guard::register(w);
    blocks::monitoring::register(w);
    blocks::auth::register(w);
    blocks::iam::register(w);
    blocks::web::register(w);
}
