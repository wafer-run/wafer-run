pub mod config;
pub mod crypto;
pub mod database;
pub mod logger;
pub mod network;
pub mod storage;

use wafer_run::common::ErrorCode;
use wafer_run::context::Context;
use wafer_run::types::*;

/// Call a block and return the raw response bytes.
/// Returns `Err(WaferError)` if the block returns an error.
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

/// Deserialize JSON bytes into a typed value.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, WaferError> {
    serde_json::from_slice(data)
        .map_err(|e| WaferError::new(ErrorCode::INTERNAL, format!("decode error: {e}")))
}
