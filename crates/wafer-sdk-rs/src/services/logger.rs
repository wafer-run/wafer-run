//! Logger service client — uses the thin ABI `log` host import directly.

use crate::log;

/// Log a message at the DEBUG level.
pub fn debug(msg: &str) {
    log("debug", msg);
}

/// Log a message at the DEBUG level with structured fields.
pub fn debug_with(msg: &str, fields: &[(&str, &str)]) {
    let formatted = format_with_fields(msg, fields);
    log("debug", &formatted);
}

/// Log a message at the INFO level.
pub fn info(msg: &str) {
    log("info", msg);
}

/// Log a message at the INFO level with structured fields.
pub fn info_with(msg: &str, fields: &[(&str, &str)]) {
    let formatted = format_with_fields(msg, fields);
    log("info", &formatted);
}

/// Log a message at the WARN level.
pub fn warn(msg: &str) {
    log("warn", msg);
}

/// Log a message at the WARN level with structured fields.
pub fn warn_with(msg: &str, fields: &[(&str, &str)]) {
    let formatted = format_with_fields(msg, fields);
    log("warn", &formatted);
}

/// Log a message at the ERROR level.
pub fn error(msg: &str) {
    log("error", msg);
}

/// Log a message at the ERROR level with structured fields.
pub fn error_with(msg: &str, fields: &[(&str, &str)]) {
    let formatted = format_with_fields(msg, fields);
    log("error", &formatted);
}

fn format_with_fields(msg: &str, fields: &[(&str, &str)]) -> String {
    if fields.is_empty() {
        return msg.to_string();
    }
    let kv: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{msg} {}", kv.join(" "))
}
