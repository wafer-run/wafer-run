//! Native `LoggerService` implementation backed by the `tracing` crate.
//!
//! Re-exports `LoggerService` and `Field` from `wafer_core::interfaces::logger`
//! so consumers can `use wafer_block_logger::service::{LoggerService, Field, TracingLogger}`.

// Re-export the trait and types from wafer-core.
pub use wafer_core::interfaces::logger::service::*;

/// `LoggerService` implementation that forwards every call to the `tracing` crate
/// at the matching level (`debug!`/`info!`/`warn!`/`error!`) with fields rendered
/// as a trailing `key=value key=value` string on the default target.
pub struct TracingLogger;

impl LoggerService for TracingLogger {
    fn debug(&self, msg: &str, fields: &[Field]) {
        let fields_str = format_fields(fields);
        tracing::debug!("{} {}", msg, fields_str);
    }

    fn info(&self, msg: &str, fields: &[Field]) {
        let fields_str = format_fields(fields);
        tracing::info!("{} {}", msg, fields_str);
    }

    fn warn(&self, msg: &str, fields: &[Field]) {
        let fields_str = format_fields(fields);
        tracing::warn!("{} {}", msg, fields_str);
    }

    fn error(&self, msg: &str, fields: &[Field]) {
        let fields_str = format_fields(fields);
        tracing::error!("{} {}", msg, fields_str);
    }
}

fn format_fields(fields: &[Field]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    fields
        .iter()
        .map(|f| format!("{}={}", f.key, f.value))
        .collect::<Vec<_>>()
        .join(" ")
}
