#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
use wafer_block::{
    common::{ErrorCode, ServiceOp},
    wire::config::{GetRequest, GetResponse, SetRequest},
    WaferError,
};

use super::{call_service, decode, dual_api, svc, svc_fn};

const BLOCK: &str = "wafer-run/config";

// ===========================================================================
// Public API — generated as async (native) or sync (wasm-component)
// ===========================================================================

dual_api! {
    /// Fetch the value of `key` from the config block.
    ///
    /// A key that is not set is `Err` with [`ErrorCode::NotFound`]. Any other
    /// `Err` (a WRAP denial, a transport or decode failure) means the read
    /// failed and says nothing about the key's value.
    pub fn get(ctx, key: &str) -> Result<String, WaferError> {
        let req = GetRequest { key: key.to_string() };
        let data = svc!(ctx, BLOCK, ServiceOp::CONFIG_GET, &req, Some(key), false, Some("config"))?;
        let resp: GetResponse = decode(&data)?;
        Ok(resp.value)
    }

    /// Like [`get`], but a key that is not set is `Ok(None)`. Every other
    /// error is returned.
    pub fn get_optional(ctx, key: &str) -> Result<Option<String>, WaferError> {
        match svc_fn!(ctx, get(key)) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.code == ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Like [`get`], but a key that is not set yields `default`. Every other
    /// error is returned, never replaced by `default`.
    pub fn get_default(ctx, key: &str, default: &str) -> Result<String, WaferError> {
        Ok(svc_fn!(ctx, get_optional(key))?.unwrap_or_else(|| default.to_string()))
    }

    /// Set `key` to `value` in the config block.
    pub fn set(ctx, key: &str, value: &str) -> Result<(), WaferError> {
        let req = SetRequest { key: key.to_string(), value: value.to_string() };
        svc!(ctx, BLOCK, ServiceOp::CONFIG_SET, &req, Some(key), true, Some("config"))?;
        Ok(())
    }
}
