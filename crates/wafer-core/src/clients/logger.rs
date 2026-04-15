use std::collections::HashMap;

use serde::Serialize;

use wafer_block::common::ServiceOp;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;

use super::call_service;

const BLOCK: &str = "wafer-run/logger";

// --- Wire-format types ---

#[derive(Serialize)]
struct LogReq<'a> {
    message: &'a str,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    fields: &'a HashMap<String, serde_json::Value>,
}

// ===========================================================================
// Native async
// ===========================================================================

#[cfg(not(feature = "wasm-component"))]
async fn log(
    ctx: &dyn Context,
    kind: &str,
    message: &str,
    fields: &HashMap<String, serde_json::Value>,
) {
    if let Err(e) = call_service(
        ctx,
        BLOCK,
        kind,
        &LogReq { message, fields },
        None,
        false,
        None,
    )
    .await
    {
        // Fall back to tracing if the logger block is unavailable.
        tracing::warn!(
            logger_error = %e,
            original_message = message,
            "logger block call failed — message may be lost"
        );
    }
}

#[cfg(not(feature = "wasm-component"))]
pub async fn debug(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_DEBUG, message, &HashMap::new()).await;
}

#[cfg(not(feature = "wasm-component"))]
pub async fn info(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_INFO, message, &HashMap::new()).await;
}

#[cfg(not(feature = "wasm-component"))]
pub async fn warn(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_WARN, message, &HashMap::new()).await;
}

#[cfg(not(feature = "wasm-component"))]
pub async fn error(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_ERROR, message, &HashMap::new()).await;
}

#[cfg(not(feature = "wasm-component"))]
pub async fn debug_with(
    ctx: &dyn Context,
    message: &str,
    fields: &HashMap<String, serde_json::Value>,
) {
    log(ctx, ServiceOp::LOGGER_DEBUG, message, fields).await;
}

#[cfg(not(feature = "wasm-component"))]
pub async fn info_with(
    ctx: &dyn Context,
    message: &str,
    fields: &HashMap<String, serde_json::Value>,
) {
    log(ctx, ServiceOp::LOGGER_INFO, message, fields).await;
}

#[cfg(not(feature = "wasm-component"))]
pub async fn warn_with(
    ctx: &dyn Context,
    message: &str,
    fields: &HashMap<String, serde_json::Value>,
) {
    log(ctx, ServiceOp::LOGGER_WARN, message, fields).await;
}

#[cfg(not(feature = "wasm-component"))]
pub async fn error_with(
    ctx: &dyn Context,
    message: &str,
    fields: &HashMap<String, serde_json::Value>,
) {
    log(ctx, ServiceOp::LOGGER_ERROR, message, fields).await;
}

// ===========================================================================
// WASM sync — falls back to WIT runtime::log on failure
// ===========================================================================

#[cfg(feature = "wasm-component")]
fn log(kind: &str, message: &str, fields: &HashMap<String, serde_json::Value>) {
    // Best-effort: attempt to call the logger block. Ignore errors silently.
    let _ = call_service(BLOCK, kind, &LogReq { message, fields }, None, false, None);
}

#[cfg(feature = "wasm-component")]
pub fn debug(message: &str) {
    log(ServiceOp::LOGGER_DEBUG, message, &HashMap::new());
}

#[cfg(feature = "wasm-component")]
pub fn info(message: &str) {
    log(ServiceOp::LOGGER_INFO, message, &HashMap::new());
}

#[cfg(feature = "wasm-component")]
pub fn warn(message: &str) {
    log(ServiceOp::LOGGER_WARN, message, &HashMap::new());
}

#[cfg(feature = "wasm-component")]
pub fn error(message: &str) {
    log(ServiceOp::LOGGER_ERROR, message, &HashMap::new());
}

#[cfg(feature = "wasm-component")]
pub fn debug_with(message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ServiceOp::LOGGER_DEBUG, message, fields);
}

#[cfg(feature = "wasm-component")]
pub fn info_with(message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ServiceOp::LOGGER_INFO, message, fields);
}

#[cfg(feature = "wasm-component")]
pub fn warn_with(message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ServiceOp::LOGGER_WARN, message, fields);
}

#[cfg(feature = "wasm-component")]
pub fn error_with(message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ServiceOp::LOGGER_ERROR, message, fields);
}
