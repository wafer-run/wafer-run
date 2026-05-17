use std::collections::HashMap;

#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
use wafer_block::{common::ServiceOp, wire::logger::LogRequest};

use super::call_service;

const BLOCK: &str = "wafer-run/logger";

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
    let req = LogRequest {
        message: message.to_string(),
        fields: fields.clone(),
    };
    if let Err(e) = call_service(ctx, BLOCK, kind, &req, None, false, None).await {
        // Fall back to tracing if the logger block is unavailable.
        tracing::warn!(
            logger_error = %e,
            original_message = message,
            "logger block call failed — message may be lost"
        );
    }
}

/// Emit a debug-level log line via the logger block.
#[cfg(not(feature = "wasm-component"))]
pub async fn debug(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_DEBUG, message, &HashMap::new()).await;
}

/// Emit an info-level log line via the logger block.
#[cfg(not(feature = "wasm-component"))]
pub async fn info(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_INFO, message, &HashMap::new()).await;
}

/// Emit a warn-level log line via the logger block.
#[cfg(not(feature = "wasm-component"))]
pub async fn warn(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_WARN, message, &HashMap::new()).await;
}

/// Emit an error-level log line via the logger block.
#[cfg(not(feature = "wasm-component"))]
pub async fn error(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_ERROR, message, &HashMap::new()).await;
}

/// Emit a debug-level log line with structured fields attached.
#[cfg(not(feature = "wasm-component"))]
pub async fn debug_with(
    ctx: &dyn Context,
    message: &str,
    fields: &HashMap<String, serde_json::Value>,
) {
    log(ctx, ServiceOp::LOGGER_DEBUG, message, fields).await;
}

/// Emit an info-level log line with structured fields attached.
#[cfg(not(feature = "wasm-component"))]
pub async fn info_with(
    ctx: &dyn Context,
    message: &str,
    fields: &HashMap<String, serde_json::Value>,
) {
    log(ctx, ServiceOp::LOGGER_INFO, message, fields).await;
}

/// Emit a warn-level log line with structured fields attached.
#[cfg(not(feature = "wasm-component"))]
pub async fn warn_with(
    ctx: &dyn Context,
    message: &str,
    fields: &HashMap<String, serde_json::Value>,
) {
    log(ctx, ServiceOp::LOGGER_WARN, message, fields).await;
}

/// Emit an error-level log line with structured fields attached.
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
    let req = LogRequest {
        message: message.to_string(),
        fields: fields.clone(),
    };
    let _ = call_service(BLOCK, kind, &req, None, false, None);
}

/// Emit a debug-level log line via the logger block.
#[cfg(feature = "wasm-component")]
pub fn debug(message: &str) {
    log(ServiceOp::LOGGER_DEBUG, message, &HashMap::new());
}

/// Emit an info-level log line via the logger block.
#[cfg(feature = "wasm-component")]
pub fn info(message: &str) {
    log(ServiceOp::LOGGER_INFO, message, &HashMap::new());
}

/// Emit a warn-level log line via the logger block.
#[cfg(feature = "wasm-component")]
pub fn warn(message: &str) {
    log(ServiceOp::LOGGER_WARN, message, &HashMap::new());
}

/// Emit an error-level log line via the logger block.
#[cfg(feature = "wasm-component")]
pub fn error(message: &str) {
    log(ServiceOp::LOGGER_ERROR, message, &HashMap::new());
}

/// Emit a debug-level log line with structured fields attached.
#[cfg(feature = "wasm-component")]
pub fn debug_with(message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ServiceOp::LOGGER_DEBUG, message, fields);
}

/// Emit an info-level log line with structured fields attached.
#[cfg(feature = "wasm-component")]
pub fn info_with(message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ServiceOp::LOGGER_INFO, message, fields);
}

/// Emit a warn-level log line with structured fields attached.
#[cfg(feature = "wasm-component")]
pub fn warn_with(message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ServiceOp::LOGGER_WARN, message, fields);
}

/// Emit an error-level log line with structured fields attached.
#[cfg(feature = "wasm-component")]
pub fn error_with(message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ServiceOp::LOGGER_ERROR, message, fields);
}
