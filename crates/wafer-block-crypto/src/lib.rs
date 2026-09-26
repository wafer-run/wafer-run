//! Cryptographic primitives and service implementations for WAFER.
//!
//! - [`primitives`] — pure, wasm32-safe building blocks (base64url,
//!   HMAC-SHA256, HS256 JWT sign/verify with explicit `exp` policy, HKDF
//!   per-block key derivation, argon2id and PBKDF2-HMAC-SHA256 password
//!   hashing, the argon2id-then-HMAC password pepper, constant-time
//!   comparison, CSPRNG bytes). The single source of
//!   truth for the WAFER crypto stack — consumers build thin policy
//!   wrappers over it instead of re-implementing the algorithms.
//! - [`service`] — `Argon2JwtCryptoService`, the native [`CryptoService`]
//!   implementation built on those primitives. The `CryptoService` trait is
//!   re-exported from `wafer_core::interfaces::crypto`. On a native host it
//!   hashes and verifies passwords on a dedicated thread, under any
//!   executor.
//!
//! Use `wafer_core::service_blocks::crypto::register_with()` to register.
//!
//! [`CryptoService`]: service::CryptoService

#![warn(missing_docs)]

#[cfg(not(target_arch = "wasm32"))]
mod offload;
pub mod primitives;

/// `Argon2JwtCryptoService`: the native `CryptoService` implementation —
/// a thin policy wrapper (a selectable password scheme defaulting to
/// argon2id at default cost, HS256 JWT with required `exp`, HKDF per-block
/// keys) over [`primitives`].
pub mod service;
