//! Logger service client using WIT-generated imports.

use crate::wafer::block_world::logger as wit;

/// Log a message at the DEBUG level.
pub fn debug(msg: &str) {
    wit::debug(msg, &[]);
}

/// Log a message at the DEBUG level with structured fields.
pub fn debug_with(msg: &str, fields: &[(&str, &str)]) {
    let wit_fields: Vec<wit::LogField> = fields.iter()
        .map(|(k, v)| wit::LogField { key: k.to_string(), value: v.to_string() })
        .collect();
    wit::debug(msg, &wit_fields);
}

/// Log a message at the INFO level.
pub fn info(msg: &str) {
    wit::info(msg, &[]);
}

/// Log a message at the INFO level with structured fields.
pub fn info_with(msg: &str, fields: &[(&str, &str)]) {
    let wit_fields: Vec<wit::LogField> = fields.iter()
        .map(|(k, v)| wit::LogField { key: k.to_string(), value: v.to_string() })
        .collect();
    wit::info(msg, &wit_fields);
}

/// Log a message at the WARN level.
pub fn warn(msg: &str) {
    wit::warn(msg, &[]);
}

/// Log a message at the WARN level with structured fields.
pub fn warn_with(msg: &str, fields: &[(&str, &str)]) {
    let wit_fields: Vec<wit::LogField> = fields.iter()
        .map(|(k, v)| wit::LogField { key: k.to_string(), value: v.to_string() })
        .collect();
    wit::warn(msg, &wit_fields);
}

/// Log a message at the ERROR level.
pub fn error(msg: &str) {
    wit::error(msg, &[]);
}

/// Log a message at the ERROR level with structured fields.
pub fn error_with(msg: &str, fields: &[(&str, &str)]) {
    let wit_fields: Vec<wit::LogField> = fields.iter()
        .map(|(k, v)| wit::LogField { key: k.to_string(), value: v.to_string() })
        .collect();
    wit::error(msg, &wit_fields);
}
