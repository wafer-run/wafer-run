//! Cryptographic service implementations for WAFER.
//!
//! Provides `Argon2JwtCryptoService` (argon2id + HMAC-SHA256 JWT) for native use.
//! The `CryptoService` trait is re-exported from `wafer_core::interfaces::crypto`.
//!
//! Use `wafer_core::service_blocks::crypto::register_with()` to register.

#![warn(missing_docs)]

/// Concrete `CryptoService` implementation: argon2id password hashing and
/// HMAC-SHA256 (HS256) JWT signing, with per-block keys derived via HKDF-SHA256.
pub mod service;
