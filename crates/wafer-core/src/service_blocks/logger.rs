use std::sync::Arc;

use crate::interfaces::logger::{
    handler,
    service::{Field, LoggerService},
};

crate::service_block! {
    /// Unified logger block. Wraps any `LoggerService` implementation.
    block: pub LoggerBlock,
    name: "wafer-run/logger",
    version: "0.0.1",
    interface: "logger@v1",
    description: "Structured logging",
    category: Service,
    fields: { service: Arc<dyn LoggerService> },
    handle: |this, _ctx, msg, body| handler::handle_message(this.service.as_ref(), &msg, &body),
}

/// [`LoggerService`] implementation that forwards every call to the `tracing`
/// crate at the matching level (`debug!`/`info!`/`warn!`/`error!`) with fields
/// rendered as a trailing `key=value key=value` string on the default target.
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
