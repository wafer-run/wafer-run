use std::collections::HashMap;

use serde::Serialize;

use wafer_run::common::ServiceOp;
use wafer_run::context::Context;

use super::call_service;

const BLOCK: &str = "wafer/logger";

// --- Wire-format types ---

#[derive(Serialize)]
struct LogReq<'a> {
    message: &'a str,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    fields: &'a HashMap<String, serde_json::Value>,
}

// --- Public API ---

fn log(ctx: &dyn Context, kind: &str, message: &str, fields: &HashMap<String, serde_json::Value>) {
    let _ = call_service(ctx, BLOCK, kind, &LogReq { message, fields });
}

pub fn debug(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_DEBUG, message, &HashMap::new());
}

pub fn info(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_INFO, message, &HashMap::new());
}

pub fn warn(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_WARN, message, &HashMap::new());
}

pub fn error(ctx: &dyn Context, message: &str) {
    log(ctx, ServiceOp::LOGGER_ERROR, message, &HashMap::new());
}

pub fn debug_with(ctx: &dyn Context, message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ctx, ServiceOp::LOGGER_DEBUG, message, fields);
}

pub fn info_with(ctx: &dyn Context, message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ctx, ServiceOp::LOGGER_INFO, message, fields);
}

pub fn warn_with(ctx: &dyn Context, message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ctx, ServiceOp::LOGGER_WARN, message, fields);
}

pub fn error_with(ctx: &dyn Context, message: &str, fields: &HashMap<String, serde_json::Value>) {
    log(ctx, ServiceOp::LOGGER_ERROR, message, fields);
}
