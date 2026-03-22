use serde::{Deserialize, Serialize};

use wafer_run::common::ServiceOp;
#[cfg(not(target_arch = "wasm32"))]
use wafer_run::context::Context;
use wafer_run::types::WaferError;

use super::{call_service, decode};

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
// Public API — native async
// ===========================================================================

#[cfg(not(target_arch = "wasm32"))]
pub async fn get(ctx: &dyn Context, key: &str) -> Result<String, WaferError> {
    let data = call_service(ctx, BLOCK, ServiceOp::CONFIG_GET, &GetReq { key }).await?;
    let resp: GetResp = decode(&data)?;
    Ok(resp.value)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn get_default(ctx: &dyn Context, key: &str, default: &str) -> String {
    get(ctx, key).await.unwrap_or_else(|_| default.to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn set(ctx: &dyn Context, key: &str, value: &str) -> Result<(), WaferError> {
    call_service(ctx, BLOCK, ServiceOp::CONFIG_SET, &SetReq { key, value }).await?;
    Ok(())
}

// ===========================================================================
// Public API — WASM sync
// ===========================================================================

#[cfg(target_arch = "wasm32")]
pub fn get(key: &str) -> Result<String, WaferError> {
    let data = call_service(BLOCK, ServiceOp::CONFIG_GET, &GetReq { key })?;
    let resp: GetResp = decode(&data)?;
    Ok(resp.value)
}

#[cfg(target_arch = "wasm32")]
pub fn get_default(key: &str, default: &str) -> String {
    get(key).unwrap_or_else(|_| default.to_string())
}

#[cfg(target_arch = "wasm32")]
pub fn set(key: &str, value: &str) -> Result<(), WaferError> {
    call_service(BLOCK, ServiceOp::CONFIG_SET, &SetReq { key, value })?;
    Ok(())
}
