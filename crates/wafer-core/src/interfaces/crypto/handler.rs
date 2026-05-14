//! Shared message handler logic for the crypto block.

use std::time::Duration;

use wafer_block::{
    common::{ErrorCode, ServiceOp},
    meta::META_WRAP_RESOURCE,
    streams::output::OutputStream,
    wire::crypto as wire,
    *,
};

use super::service::{CryptoError, CryptoService};
use crate::interfaces::handler_util::{decode_or_err, to_output};

/// SEC-003: enforce that the caller-supplied `wrap.resource` meta matches the
/// crypto operation about to run. If the meta is absent the runtime skipped
/// WRAP — accept; client wrappers always set this post-SEC-015.
fn check_op(msg: &Message, expected: &str) -> Result<(), WaferError> {
    let supplied = msg.get_meta(META_WRAP_RESOURCE);
    if supplied.is_empty() || supplied == expected {
        Ok(())
    } else {
        Err(WaferError::new(
            ErrorCode::PERMISSION_DENIED,
            format!("WRAP: wrap.resource meta '{supplied}' does not match crypto op '{expected}'"),
        ))
    }
}

// --- Helpers ---

fn crypto_error_to_wafer(e: CryptoError) -> WaferError {
    match e {
        CryptoError::HashError(msg) => WaferError::new(ErrorCode::INTERNAL, msg),
        CryptoError::PasswordMismatch => {
            WaferError::new(ErrorCode::UNAUTHENTICATED, "password mismatch")
        }
        CryptoError::SignError(msg) => WaferError::new(ErrorCode::INTERNAL, msg),
        CryptoError::VerifyError(msg) => WaferError::new(ErrorCode::UNAUTHENTICATED, msg),
        CryptoError::Other(msg) => WaferError::new(ErrorCode::INTERNAL, msg),
    }
}

/// Handle a crypto message by delegating to the given service.
///
/// JWT sign/verify use per-block HKDF-derived keys when `caller_id` is set
/// (the runtime provides this from the calling block's identity).
/// When `caller_id` is None, the master key is used.
pub fn handle_message(
    service: &dyn CryptoService,
    caller_id: Option<&str>,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::CRYPTO_HASH => {
            if let Err(e) = check_op(msg, "hash") {
                return OutputStream::error(e);
            }
            let req = decode_or_err!(body, wire::HashRequest, "crypto.hash");
            match service.hash(&req.password) {
                Ok(hash) => to_output(&wire::HashResponse { hash }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_COMPARE_HASH => {
            if let Err(e) = check_op(msg, "compare_hash") {
                return OutputStream::error(e);
            }
            let req = decode_or_err!(body, wire::CompareHashRequest, "crypto.compare_hash");
            match service.compare_hash(&req.password, &req.hash) {
                Ok(()) => to_output(&wire::CompareHashResponse { matches: true }),
                Err(CryptoError::PasswordMismatch) => {
                    to_output(&wire::CompareHashResponse { matches: false })
                }
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_SIGN => {
            if let Err(e) = check_op(msg, "sign") {
                return OutputStream::error(e);
            }
            let req = decode_or_err!(body, wire::SignRequest, "crypto.sign");
            let expiry = Duration::from_secs(req.expiry_secs);
            let result = match caller_id {
                Some(id) => service.sign_for(id, req.claims, expiry),
                None => service.sign(req.claims, expiry),
            };
            match result {
                Ok(token) => to_output(&wire::SignResponse { token }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_VERIFY => {
            if let Err(e) = check_op(msg, "verify") {
                return OutputStream::error(e);
            }
            let req = decode_or_err!(body, wire::VerifyRequest, "crypto.verify");
            let result = match caller_id {
                Some(id) => service.verify_for(id, &req.token),
                None => service.verify(&req.token),
            };
            match result {
                Ok(claims) => to_output(&wire::VerifyResponse { claims }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_RANDOM_BYTES => {
            if let Err(e) = check_op(msg, "random_bytes") {
                return OutputStream::error(e);
            }
            let req = decode_or_err!(body, wire::RandomBytesRequest, "crypto.random_bytes");
            const MAX_RANDOM_BYTES: usize = 1_048_576;
            if req.n > MAX_RANDOM_BYTES {
                return OutputStream::error(WaferError::new(
                    ErrorCode::INVALID_ARGUMENT,
                    format!(
                        "random_bytes n={} exceeds maximum of {}",
                        req.n, MAX_RANDOM_BYTES
                    ),
                ));
            }
            match service.random_bytes(req.n) {
                Ok(bytes) => to_output(&wire::RandomBytesResponse { bytes }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown crypto operation: {other}"),
        )),
    }
}
