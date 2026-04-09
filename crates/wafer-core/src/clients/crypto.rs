use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use wafer_block::common::{ErrorCode, ServiceOp};
#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
use wafer_block::WaferError;

use super::{call_service, decode, dual_api, svc};

const BLOCK: &str = "wafer-run/crypto";

// --- Wire-format types ---

#[derive(Serialize)]
struct HashReq<'a> {
    password: &'a str,
}

#[derive(Deserialize)]
struct HashResp {
    hash: String,
}

#[derive(Serialize)]
struct CompareHashReq<'a> {
    password: &'a str,
    hash: &'a str,
}

#[derive(Deserialize)]
struct CompareHashResp {
    #[serde(rename = "match")]
    matches: bool,
}

#[derive(Serialize)]
struct SignReq<'a> {
    claims: &'a HashMap<String, serde_json::Value>,
    expiry_secs: u64,
}

#[derive(Deserialize)]
struct SignResp {
    token: String,
}

#[derive(Serialize)]
struct VerifyReq<'a> {
    token: &'a str,
}

#[derive(Deserialize)]
struct VerifyResp {
    claims: HashMap<String, serde_json::Value>,
}

#[derive(Serialize)]
struct RandomBytesReq {
    n: usize,
}

#[derive(Deserialize)]
struct RandomBytesResp {
    bytes: Vec<u8>,
}

// ===========================================================================
// Public API — generated as async (native) or sync (wasm-component)
// ===========================================================================

dual_api! {
    pub fn hash(ctx, password: &str) -> Result<String, WaferError> {
        let data = svc!(ctx, BLOCK, ServiceOp::CRYPTO_HASH, &HashReq { password }, None, false, Some("crypto"))?;
        let resp: HashResp = decode(&data)?;
        Ok(resp.hash)
    }

    pub fn compare_hash(ctx, password: &str, hash: &str) -> Result<(), WaferError> {
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::CRYPTO_COMPARE_HASH,
            &CompareHashReq { password, hash },
            None,
            false,
            Some("crypto")
        )?;
        let resp: CompareHashResp = decode(&data)?;
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
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::CRYPTO_SIGN,
            &SignReq { claims, expiry_secs: expiry.as_secs() },
            None,
            false,
            Some("crypto")
        )?;
        let resp: SignResp = decode(&data)?;
        Ok(resp.token)
    }

    pub fn verify(ctx, token: &str) -> Result<HashMap<String, serde_json::Value>, WaferError> {
        let data = svc!(ctx, BLOCK, ServiceOp::CRYPTO_VERIFY, &VerifyReq { token }, None, false, Some("crypto"))?;
        let resp: VerifyResp = decode(&data)?;
        Ok(resp.claims)
    }

    pub fn random_bytes(ctx, n: usize) -> Result<Vec<u8>, WaferError> {
        let data = svc!(ctx, BLOCK, ServiceOp::CRYPTO_RANDOM_BYTES, &RandomBytesReq { n }, None, false, Some("crypto"))?;
        let resp: RandomBytesResp = decode(&data)?;
        Ok(resp.bytes)
    }
}
