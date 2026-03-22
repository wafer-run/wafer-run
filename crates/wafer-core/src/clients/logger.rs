use std::collections::HashMap;

use serde::Serialize;

use wafer_run::common::ServiceOp;
#[cfg(not(target_arch = "wasm32"))]
use wafer_run::context::Context;

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

#[cfg(not(target_arch = "wasm32"))]
async fn log(ctx: &dyn Context, kind: &str, message: &str, fields: &HashMap<String, serde_json::Value>) {
    if let Err(e) = call_service(ctx, BLOCK, kind, &LogReq { message, fields }).await {
        // Fall back to tracing if the logger block is unavailable.
        tracing::warn!(
            logger_error = %e,
            original_message = message,
            "logger block call failed — message may be lost"
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn debug(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_DEBUG, message, &HashMap::new()).await;
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn info(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_INFO, message, &HashMap::new()).await;
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn warn(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_WARN, message, &HashMap::new()).await;
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn error(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_ERROR, message, &HashMap::new()).await;
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn debug_with(ctx: &dyn Context, message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ctx, ServiceOp::LOGGER_DEBUG, message, fields).await;
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn info_with(ctx: &dyn Context, message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ctx, ServiceOp::LOGGER_INFO, message, fields).await;
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn warn_with(ctx: &dyn Context, message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ctx, ServiceOp::LOGGER_WARN, message, fields).await;
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn error_with(ctx: &dyn Context, message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ctx, ServiceOp::LOGGER_ERROR, message, fields).await;
}

// ===========================================================================
// WASM sync — falls back to WIT runtime::log on failure
// ===========================================================================

#[cfg(target_arch = "wasm32")]
fn log(kind: &str, message: &str, fields: &HashMap<String, serde_json::Value>) {
    if call_service(BLOCK, kind, &LogReq { message, fields }).is_err() {
        // Fall back to WIT runtime log import.
        let level = kind.strip_prefix("logger.").unwrap_or("info");
        wafer_block::runtime::log(level, message);
    }
}

#[cfg(target_arch = "wasm32")]
pub fn debug(message: &str) {
    log(ServiceOp::LOGGER_DEBUG, message, &HashMap::new());
}

#[cfg(target_arch = "wasm32")]
pub fn info(message: &str) {
    log(ServiceOp::LOGGER_INFO, message, &HashMap::new());
}

#[cfg(target_arch = "wasm32")]
pub fn warn(message: &str) {
    log(ServiceOp::LOGGER_WARN, message, &HashMap::new());
}

#[cfg(target_arch = "wasm32")]
pub fn error(message: &str) {
    log(ServiceOp::LOGGER_ERROR, message, &HashMap::new());
}

#[cfg(target_arch = "wasm32")]
pub fn debug_with(message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ServiceOp::LOGGER_DEBUG, message, fields);
}

#[cfg(target_arch = "wasm32")]
pub fn info_with(message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ServiceOp::LOGGER_INFO, message, fields);
}

#[cfg(target_arch = "wasm32")]
pub fn warn_with(message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ServiceOp::LOGGER_WARN, message, fields);
}

#[cfg(target_arch = "wasm32")]
pub fn error_with(message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ServiceOp::LOGGER_ERROR, message, fields);
}
