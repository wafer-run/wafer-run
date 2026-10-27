//! S3-compatible storage for WAFER: [`service::S3StorageService`], and — with
//! the `block` feature — the `wafer-run/s3` block wrapping it behind the
//! shared `storage@v1` message handler.
//!
//! The block registers itself with every runtime that links the crate
//! (`register_static_block!`) and reads its own config at `Init`, so it is
//! opt-in: a consumer that only needs the service, and wraps it in a storage
//! block of its own, leaves the feature off.

#![warn(missing_docs)]

pub mod service;

/// The `wafer-run/s3` block (the `block` feature).
#[cfg(feature = "block")]
mod block;
