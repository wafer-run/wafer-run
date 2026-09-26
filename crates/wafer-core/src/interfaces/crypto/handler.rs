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
        // A stored credential that cannot be checked is a server-side fault,
        // never a wrong password: `Unauthenticated` here would tell the caller
        // (and its logs) that the user mistyped.
        e @ CryptoError::MalformedHash(_) => WaferError::new(ErrorCode::Internal, e.to_string()),
        CryptoError::SignError(msg) => WaferError::new(ErrorCode::Internal, msg),
        CryptoError::VerifyError(msg) => WaferError::new(ErrorCode::Unauthenticated, msg),
        CryptoError::Other(msg) => WaferError::new(ErrorCode::Internal, msg),
    }
}

/// Tokens are keyed per calling block, so a token op without one has no key.
fn no_caller_key(op: &str) -> WaferError {
    WaferError::new(
        ErrorCode::PermissionDenied,
        format!("{op} needs a calling block: tokens are signed under the caller's derived key"),
    )
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
/// JWT sign/verify always use the per-block key derived for `caller_id`
/// (the runtime provides this from the calling block's identity). This is a
/// separate concern from WRAP enforcement above — `caller_id` selects the
/// derived key, it does not gate access. A `sign`/`verify` with no
/// `caller_id` is refused with `PermissionDenied`: there is no key to use.
///
/// Every service call is awaited, on every target: where and how the work
/// runs (a blocking pool for Argon2 on a native host, a Durable Object on a
/// Worker) is the service's decision, not this handler's.
pub async fn handle_message(
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
                    (
                        "hash".to_string(),
                        ResourceType::Crypto,
                        ResourceAccess::Read,
                    )
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            match service.hash(&req.password).await {
                Ok(hash) => to_output(&wire::HashResponse { hash }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_COMPARE_HASH => {
            let req = match decode_and_authorize::<wire::CompareHashRequest>(
                ctx,
                body,
                "crypto.compare_hash",
                |_r| {
                    (
                        "compare_hash".to_string(),
                        ResourceType::Crypto,
                        ResourceAccess::Read,
                    )
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.compare_hash(&req.password, &req.hash).await {
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
                    (
                        "sign".to_string(),
                        ResourceType::Crypto,
                        ResourceAccess::Read,
                    )
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            let Some(id) = caller_id else {
                return OutputStream::error(no_caller_key("crypto.sign"));
            };
            match service
                .sign_for(id, req.claims, Duration::from_secs(req.expiry_secs))
                .await
            {
                Ok(token) => to_output(&wire::SignResponse { token }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_VERIFY => {
            let req = match decode_and_authorize::<wire::VerifyRequest>(
                ctx,
                body,
                "crypto.verify",
                |_r| {
                    (
                        "verify".to_string(),
                        ResourceType::Crypto,
                        ResourceAccess::Read,
                    )
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let Some(id) = caller_id else {
                return OutputStream::error(no_caller_key("crypto.verify"));
            };
            match service.verify_for(id, &req.token).await {
                Ok(claims) => to_output(&wire::VerifyResponse { claims }),
                Err(e) => OutputStream::error(crypto_error_to_wafer(e)),
            }
        }
        ServiceOp::CRYPTO_RANDOM_BYTES => {
            let req = match decode_and_authorize::<wire::RandomBytesRequest>(
                ctx,
                body,
                "crypto.random_bytes",
                |_r| {
                    (
                        "random_bytes".to_string(),
                        ResourceType::Crypto,
                        ResourceAccess::Read,
                    )
                },
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
            match service.random_bytes(req.n).await {
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
