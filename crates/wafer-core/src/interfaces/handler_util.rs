//! Shared handler utilities used across service block handlers.
//!
//! Provides `to_output` for serializing response payloads (MessagePack via codec)
//! and `decode_or_err!` for deserializing request bodies with uniform error
//! handling.

use wafer_block::{codec, streams::output::OutputStream};

/// Serialize a value via codec (MessagePack) and return as `OutputStream::respond`,
/// or return an error stream if serialization fails.
pub fn to_output<T: serde::Serialize>(val: T) -> OutputStream {
    match codec::encode(&val) {
        Ok(bytes) => OutputStream::respond(bytes),
        Err(e) => OutputStream::error(e),
    }
}

/// Decode a request body via codec, returning the typed value or an error `OutputStream`.
///
/// Usage: `let req = decode_or_err!(body, MyRequest, "service.operation");`
///
/// On decode failure, returns early from the enclosing function with an
/// `OutputStream::error` containing `ErrorCode::INVALID_ARGUMENT`.
macro_rules! decode_or_err {
    ($body:expr, $ty:ty, $op_name:expr) => {
        match wafer_block::codec::decode::<$ty>($body) {
            Ok(r) => r,
            Err(e) => {
                return OutputStream::error(wafer_block::WaferError::new(
                    wafer_block::common::ErrorCode::INVALID_ARGUMENT,
                    format!("invalid {} request: {}", $op_name, e.message),
                ))
            }
        }
    };
}

pub(crate) use decode_or_err;
