use std::collections::HashMap;

#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
use wafer_block::{
    common::{ErrorCode, ServiceOp},
    wire::crypto::{
        CompareHashRequest, CompareHashResponse, HashRequest, HashResponse, RandomBytesRequest,
        RandomBytesResponse, SignRequest, SignResponse, VerifyRequest, VerifyResponse,
    },
    WaferError,
};

use super::{call_service, decode, dual_api, svc};

const BLOCK: &str = "wafer-run/crypto";

// ===========================================================================
// Public API — generated as async (native) or sync (wasm-component)
// ===========================================================================

dual_api! {
    pub fn hash(ctx, password: &str) -> Result<String, WaferError> {
        let req = HashRequest { password: password.to_string() };
        let data = svc!(ctx, BLOCK, ServiceOp::CRYPTO_HASH, &req, Some("hash"), false, Some("crypto"))?;
        let resp: HashResponse = decode(&data)?;
        Ok(resp.hash)
    }

    pub fn compare_hash(ctx, password: &str, hash: &str) -> Result<(), WaferError> {
        let req = CompareHashRequest { password: password.to_string(), hash: hash.to_string() };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::CRYPTO_COMPARE_HASH,
            &req,
            Some("compare_hash"),
            false,
            Some("crypto")
        )?;
        let resp: CompareHashResponse = decode(&data)?;
        if resp.matches {
            Ok(())
        } else {
            Err(WaferError::new(ErrorCode::UNAUTHENTICATED, "password mismatch"))
        }
    }

    pub fn sign(
        ctx,
        claims: &HashMap<String, serde_json::Value>,
        expiry: std::time::Duration,
    ) -> Result<String, WaferError> {
        let req = SignRequest { claims: claims.clone(), expiry_secs: expiry.as_secs() };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::CRYPTO_SIGN,
            &req,
            Some("sign"),
            false,
            Some("crypto")
        )?;
        let resp: SignResponse = decode(&data)?;
        Ok(resp.token)
    }

    pub fn verify(ctx, token: &str) -> Result<HashMap<String, serde_json::Value>, WaferError> {
        let req = VerifyRequest { token: token.to_string() };
        let data = svc!(ctx, BLOCK, ServiceOp::CRYPTO_VERIFY, &req, Some("verify"), false, Some("crypto"))?;
        let resp: VerifyResponse = decode(&data)?;
        Ok(resp.claims)
    }

    pub fn random_bytes(ctx, n: usize) -> Result<Vec<u8>, WaferError> {
        let req = RandomBytesRequest { n };
        let data = svc!(ctx, BLOCK, ServiceOp::CRYPTO_RANDOM_BYTES, &req, Some("random_bytes"), false, Some("crypto"))?;
        let resp: RandomBytesResponse = decode(&data)?;
        Ok(resp.bytes)
    }
}
