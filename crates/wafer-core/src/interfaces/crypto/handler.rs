//! Shared message handler logic for the crypto block.

use std::time::Duration;

use wafer_block::{
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    types::ResourceType,
    wire::crypto as wire,
    *,
};

use super::service::{CryptoError, CryptoService};
use crate::interfaces::handler_util::{decode_and_authorize, to_output};

// --- Helpers ---

fn crypto_error_to_wafer(e: CryptoError) -> WaferError {
    match e {
        CryptoError::HashError(msg) => WaferError::new(ErrorCode::Internal, msg),
        CryptoError::PasswordMismatch => {
            WaferError::new(ErrorCode::Unauthenticated, "password mismatch")
        }
        CryptoError::SignError(msg) => WaferError::new(ErrorCode::Internal, msg),
        CryptoError::VerifyError(msg) => WaferError::new(ErrorCode::Unauthenticated, msg),
        CryptoError::Other(msg) => WaferError::new(ErrorCode::Internal, msg),
    }
}

/// Handle a crypto message by delegating to the given service.
///
/// `ctx` is the trusted host-side authorization surface: every op arm
/// authorizes via [`decode_and_authorize`], which bundles the codec decode
/// with a call to `ctx.check_resource_access` so an arm cannot obtain its
/// typed request without also being checked. The resource named is the
/// literal op name (`"hash"`, `"sign"`, ...) — crypto grants are keyed on
/// the operation, not on request content — and `is_write` is always `false`:
/// crypto ops aren't resource writes in the WRAP sense.
///
/// JWT sign/verify use per-block HKDF-derived keys when `caller_id` is set
/// (the runtime provides this from the calling block's identity). This is a
/// separate concern from WRAP enforcement above — `caller_id` selects the
/// derived key, it does not gate access. When `caller_id` is None, the
/// master key is used.
pub fn handle_message(
    service: &dyn CryptoService,
    ctx: &dyn Context,
    caller_id: Option<&str>,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::CRYPTO_HASH => {
            let req =
                match decode_and_authorize::<wire::HashRequest>(ctx, body, "crypto.hash", |_r| {
                    ("hash".to_string(), ResourceType::Crypto, false)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            match service.hash(&req.password) {
                Ok(hash) => to_output(&wire::HashResponse { hash }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_COMPARE_HASH => {
            let req = match decode_and_authorize::<wire::CompareHashRequest>(
                ctx,
                body,
                "crypto.compare_hash",
                |_r| ("compare_hash".to_string(), ResourceType::Crypto, false),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.compare_hash(&req.password, &req.hash) {
                Ok(()) => to_output(&wire::CompareHashResponse { matches: true }),
                Err(CryptoError::PasswordMismatch) => {
                    to_output(&wire::CompareHashResponse { matches: false })
                }
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_SIGN => {
            let req =
                match decode_and_authorize::<wire::SignRequest>(ctx, body, "crypto.sign", |_r| {
                    ("sign".to_string(), ResourceType::Crypto, false)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
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
            let req = match decode_and_authorize::<wire::VerifyRequest>(
                ctx,
                body,
                "crypto.verify",
                |_r| ("verify".to_string(), ResourceType::Crypto, false),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
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
            let req = match decode_and_authorize::<wire::RandomBytesRequest>(
                ctx,
                body,
                "crypto.random_bytes",
                |_r| ("random_bytes".to_string(), ResourceType::Crypto, false),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            const MAX_RANDOM_BYTES: usize = 1_048_576;
            if req.n > MAX_RANDOM_BYTES {
                return OutputStream::error(WaferError::new(
                    ErrorCode::InvalidArgument,
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
            ErrorCode::Unimplemented,
            format!("unknown crypto operation: {other}"),
        )),
    }
}
