use serde::{Deserialize, Serialize};
#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
use wafer_block::{common::ServiceOp, WaferError};

use super::{call_service, decode, dual_api, svc, svc_fn};

const BLOCK: &str = "wafer-run/config";

// --- Wire-format types ---

#[derive(Serialize)]
struct GetReq<'a> {
    key: &'a str,
}

#[derive(Deserialize)]
struct GetResp {
    value: String,
}

#[derive(Serialize)]
struct SetReq<'a> {
    key: &'a str,
    value: &'a str,
}

// ===========================================================================
// Public API — generated as async (native) or sync (wasm-component)
// ===========================================================================

dual_api! {
    pub fn get(ctx, key: &str) -> Result<String, WaferError> {
        let data = svc!(ctx, BLOCK, ServiceOp::CONFIG_GET, &GetReq { key }, Some(key), false, Some("config"))?;
        let resp: GetResp = decode(&data)?;
        Ok(resp.value)
    }

    pub fn get_default(ctx, key: &str, default: &str) -> String {
        svc_fn!(ctx, get(key)).unwrap_or_else(|_| default.to_string())
    }

    pub fn set(ctx, key: &str, value: &str) -> Result<(), WaferError> {
        svc!(ctx, BLOCK, ServiceOp::CONFIG_SET, &SetReq { key, value }, Some(key), true, Some("config"))?;
        Ok(())
    }
}
