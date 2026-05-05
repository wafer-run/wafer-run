//! Typed client for the logger service.
//!
//! All four verbs (`debug`, `info`, `warn`, `error`) share a single
//! [`LogRequest`] payload — only the op (and therefore `Message::kind`)
//! distinguishes the level. Logger calls are buffered and fire-and-forget;
//! the response is an empty acknowledgement.

use wafer_block::{codec, wire::logger::LogRequest, ServiceOp, WaferError};

use super::common::{consume_ack, open_buffered};

const BLOCK: &str = "wafer-run/logger";

/// Buffered: log at debug level. The response is an empty acknowledgement.
pub fn debug(request: &LogRequest) -> Result<(), WaferError> {
    log_at(ServiceOp::LOGGER_DEBUG, request)
}

/// Buffered: log at info level. The response is an empty acknowledgement.
pub fn info(request: &LogRequest) -> Result<(), WaferError> {
    log_at(ServiceOp::LOGGER_INFO, request)
}

/// Buffered: log at warn level. The response is an empty acknowledgement.
pub fn warn(request: &LogRequest) -> Result<(), WaferError> {
    log_at(ServiceOp::LOGGER_WARN, request)
}

/// Buffered: log at error level. The response is an empty acknowledgement.
pub fn error(request: &LogRequest) -> Result<(), WaferError> {
    log_at(ServiceOp::LOGGER_ERROR, request)
}

fn log_at(op: &str, request: &LogRequest) -> Result<(), WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, op, &req_bytes)?;
    consume_ack(&mut response_stream)
}
