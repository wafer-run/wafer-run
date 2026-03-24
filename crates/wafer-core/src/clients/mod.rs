pub mod config;
pub mod crypto;
pub mod database;
pub mod logger;
pub mod network;
pub mod storage;

use wafer_run::common::ErrorCode;
#[cfg(not(feature = "wasm-component"))]
use wafer_run::context::Context;
use wafer_run::types::*;

/// Call a block and return the raw response bytes (native async variant).
/// Returns `Err(WaferError)` if the block returns an error.
#[cfg(not(feature = "wasm-component"))]
pub(crate) async fn call_service(
    ctx: &dyn Context,
    block: &str,
    kind: &str,
    data: &impl serde::Serialize,
) -> Result<Vec<u8>, WaferError> {
    let payload = serde_json::to_vec(data)
        .map_err(|e| WaferError::new(ErrorCode::INTERNAL, e.to_string()))?;
    let mut msg = Message::new(kind, payload);
    let result = ctx.call_block(block, &mut msg).await;
    match result.action {
        Action::Error => Err(result
            .error
            .unwrap_or_else(|| WaferError::new(ErrorCode::INTERNAL, "unknown error"))),
        _ => Ok(result.response.map(|r| r.data).unwrap_or_default()),
    }
}

/// Call a block and return the raw response bytes (WASM sync variant).
/// Uses the WIT `runtime::call-block` host import instead of `ctx.call_block()`.
#[cfg(feature = "wasm-component")]
pub(crate) fn call_service(
    block: &str,
    kind: &str,
    data: &impl serde::Serialize,
) -> Result<Vec<u8>, WaferError> {
    let payload = serde_json::to_vec(data)
        .map_err(|e| WaferError::new(ErrorCode::INTERNAL, e.to_string()))?;
    let msg = Message::new(kind, payload);
    let result = wafer_block::runtime::call_block(block, &msg);
    match result.action {
        Action::Error => Err(result
            .error
            .unwrap_or_else(|| WaferError::new(ErrorCode::INTERNAL, "unknown error"))),
        _ => Ok(result.response.map(|r| r.data).unwrap_or_default()),
    }
}

/// Deserialize JSON bytes into a typed value.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, WaferError> {
    serde_json::from_slice(data)
        .map_err(|e| WaferError::new(ErrorCode::INTERNAL, format!("decode error: {e}")))
}
