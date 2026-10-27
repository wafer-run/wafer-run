//! PostgreSQL for WAFER: [`service::PostgresDatabaseService`], and — with the
//! `block` feature — the `wafer-run/postgres` block wrapping it behind the
//! shared `database@v1` message handler.
//!
//! The block registers itself with every runtime that links the crate
//! (`register_static_block!`) and requires its own connection URL at `Init`,
//! so it is opt-in: a consumer that only needs the service, and wraps it in
//! a database block of its own, leaves the feature off.

#![warn(missing_docs)]

mod errors;
mod params;
/// PostgreSQL implementation of `wafer_core::interfaces::database::service::DatabaseService`.
///
/// Exposed publicly so native consumers (e.g. a native application build) can construct
/// the service directly from a connection URL when running outside the
/// block lifecycle.
pub mod service;

/// The `wafer-run/postgres` block (the `block` feature).
#[cfg(feature = "block")]
mod block;
