//! Typed client for the crypto service.
//!
//! All five ops are buffered single-frame request/response, going through
//! [`super::common`]'s `call` helper: encode the typed request, open a
//! [`CallStream`](crate::stream::CallStream), decode a single response
//! frame.

use wafer_block::{
    wire::crypto::{
        CompareHashRequest, CompareHashResponse, HashRequest, HashResponse, RandomBytesRequest,
        RandomBytesResponse, SignRequest, SignResponse, VerifyRequest, VerifyResponse,
    },
    ServiceOp, WaferError,
};

use super::common::call;

const BLOCK: &str = "wafer-run/crypto";

/// Buffered: hash a password (e.g. argon2id). Returns the encoded hash.
pub fn hash(request: &HashRequest) -> Result<HashResponse, WaferError> {
    call(BLOCK, ServiceOp::CRYPTO_HASH, request)
}

/// Buffered: compare a plaintext password against a previously produced
/// hash. Returns whether they match.
pub fn compare_hash(request: &CompareHashRequest) -> Result<CompareHashResponse, WaferError> {
    call(BLOCK, ServiceOp::CRYPTO_COMPARE_HASH, request)
}

/// Buffered: sign a JWT-style claim set, returning the encoded token.
pub fn sign(request: &SignRequest) -> Result<SignResponse, WaferError> {
    call(BLOCK, ServiceOp::CRYPTO_SIGN, request)
}

/// Buffered: verify a token and return its decoded claims.
pub fn verify(request: &VerifyRequest) -> Result<VerifyResponse, WaferError> {
    call(BLOCK, ServiceOp::CRYPTO_VERIFY, request)
}

/// Buffered: generate `n` cryptographically random bytes.
pub fn random_bytes(request: &RandomBytesRequest) -> Result<RandomBytesResponse, WaferError> {
    call(BLOCK, ServiceOp::CRYPTO_RANDOM_BYTES, request)
}
