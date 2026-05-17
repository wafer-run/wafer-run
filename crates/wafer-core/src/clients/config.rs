#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
use wafer_block::{
    common::ServiceOp,
    wire::config::{GetRequest, GetResponse, SetRequest},
    WaferError,
};

use super::{call_service, decode, dual_api, svc, svc_fn};

const BLOCK: &str = "wafer-run/config";

// ===========================================================================
// Public API — generated as async (native) or sync (wasm-component)
// ===========================================================================

dual_api! {
    /// Fetch the typed value for `key` from the config block.
    /// Returns `Err(WaferError)` if the key is not set or the lookup fails.
    pub fn get(ctx, key: &str) -> Result<String, WaferError> {
        let req = GetRequest { key: key.to_string() };
        let data = svc!(ctx, BLOCK, ServiceOp::CONFIG_GET, &req, Some(key), false, Some("config"))?;
        let resp: GetResponse = decode(&data)?;
        Ok(resp.value)
    }

    /// Like [`get`], but returns `default` (cloned) on any error rather than propagating it.
    pub fn get_default(ctx, key: &str, default: &str) -> String {
        svc_fn!(ctx, get(key)).unwrap_or_else(|_| default.to_string())
    }

    /// Set `key` to `value` in the config block.
    pub fn set(ctx, key: &str, value: &str) -> Result<(), WaferError> {
        let req = SetRequest { key: key.to_string(), value: value.to_string() };
        svc!(ctx, BLOCK, ServiceOp::CONFIG_SET, &req, Some(key), true, Some("config"))?;
        Ok(())
    }
}
