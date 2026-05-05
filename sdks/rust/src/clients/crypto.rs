//! Typed client for the crypto service.
//!
//! All five ops are buffered single-frame request/response. Each fn encodes
//! its typed request, opens a [`CallStream`](crate::stream::CallStream) via
//! [`super::common::open_buffered`], then decodes a single response frame.

use wafer_block::{
    codec,
    wire::crypto::{
        CompareHashRequest, CompareHashResponse, HashRequest, HashResponse, RandomBytesRequest,
        RandomBytesResponse, SignRequest, SignResponse, VerifyRequest, VerifyResponse,
    },
    ServiceOp, WaferError,
};

use super::common::{collect_single_frame, open_buffered};

const BLOCK: &str = "wafer-run/crypto";

/// Buffered: hash a password (e.g. argon2id). Returns the encoded hash.
pub fn hash(request: &HashRequest) -> Result<HashResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::CRYPTO_HASH, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "crypto HASH")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding crypto HASH response: {}", e.message),
        )
    })
}

/// Buffered: compare a plaintext password against a previously produced
/// hash. Returns whether they match.
pub fn compare_hash(request: &CompareHashRequest) -> Result<CompareHashResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::CRYPTO_COMPARE_HASH, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "crypto COMPARE_HASH")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding crypto COMPARE_HASH response: {}", e.message),
        )
    })
}

/// Buffered: sign a JWT-style claim set, returning the encoded token.
pub fn sign(request: &SignRequest) -> Result<SignResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::CRYPTO_SIGN, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "crypto SIGN")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding crypto SIGN response: {}", e.message),
        )
    })
}

/// Buffered: verify a token and return its decoded claims.
pub fn verify(request: &VerifyRequest) -> Result<VerifyResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::CRYPTO_VERIFY, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "crypto VERIFY")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding crypto VERIFY response: {}", e.message),
        )
    })
}

/// Buffered: generate `n` cryptographically random bytes.
pub fn random_bytes(request: &RandomBytesRequest) -> Result<RandomBytesResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::CRYPTO_RANDOM_BYTES, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "crypto RANDOM_BYTES")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding crypto RANDOM_BYTES response: {}", e.message),
        )
    })
}
