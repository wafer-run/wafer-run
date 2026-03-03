use serde::{Deserialize, Serialize};

use wafer_run::common::ServiceOp;
use wafer_run::context::Context;
use wafer_run::types::WaferError;

use super::{call_service, decode};

const BLOCK: &str = "wafer/config";

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

// --- Public API ---

/// Get a config value by key. Returns `Err` with `NOT_FOUND` if the key does not exist.
pub fn get(ctx: &dyn Context, key: &str) -> Result<String, WaferError> {
    let data = call_service(ctx, BLOCK, ServiceOp::CONFIG_GET, &GetReq { key })?;
    let resp: GetResp = decode(&data)?;
    Ok(resp.value)
}

/// Get a config value, returning `default` if the key does not exist.
pub fn get_default(ctx: &dyn Context, key: &str, default: &str) -> String {
    get(ctx, key).unwrap_or_else(|_| default.to_string())
}

/// Set a config value.
pub fn set(ctx: &dyn Context, key: &str, value: &str) -> Result<(), WaferError> {
    call_service(ctx, BLOCK, ServiceOp::CONFIG_SET, &SetReq { key, value })?;
    Ok(())
}
