//! Shared handler utilities used across service block handlers.
//!
//! Provides `to_output` for serializing response payloads and `decode_or_err!`
//! for deserializing request bodies with uniform error handling.

use wafer_block::common::ErrorCode;
use wafer_block::streams::output::OutputStream;
use wafer_block::WaferError;

/// Serialize a value to JSON bytes and return as an `OutputStream::respond`,
/// or return an error stream if serialization fails.
pub fn to_output<T: serde::Serialize>(val: T) -> OutputStream {
    match serde_json::to_vec(&val) {
        Ok(bytes) => OutputStream::respond(bytes),
        Err(e) => OutputStream::error(WaferError::new(
            ErrorCode::INTERNAL,
            format!("serialize response: {e}"),
        )),
    }
}

/// Decode a request body from JSON, returning the typed value or an error `OutputStream`.
///
/// Usage: `let req = decode_or_err!(body, MyRequest, "service.operation");`
///
/// On decode failure, returns early from the enclosing function with an
/// `OutputStream::error` containing `ErrorCode::INVALID_ARGUMENT`.
macro_rules! decode_or_err {
    ($body:expr, $ty:ty, $op_name:expr) => {
        match serde_json::from_slice::<$ty>($body) {
            Ok(r) => r,
            Err(e) => {
                return OutputStream::error(wafer_block::WaferError::new(
                    wafer_block::common::ErrorCode::INVALID_ARGUMENT,
                    format!("invalid {} request: {e}", $op_name),
                ))
            }
        }
    };
}

pub(crate) use decode_or_err;
