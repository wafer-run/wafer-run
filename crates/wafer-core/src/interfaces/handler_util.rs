//! Shared handler utilities used across service block handlers.
//!
//! Provides `to_output` for serializing response payloads (MessagePack via codec)
//! and `decode_or_err!` for deserializing request bodies with uniform error
//! handling.

use wafer_block::{
    codec, common::ErrorCode, meta::META_WRAP_RESOURCE, streams::output::OutputStream, Message,
    WaferError,
};

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
/// `OutputStream::error` containing `ErrorCode::InvalidArgument`.
macro_rules! decode_or_err {
    ($body:expr, $ty:ty, $op_name:expr) => {
        match wafer_block::codec::decode::<$ty>($body) {
            Ok(r) => r,
            Err(e) => {
                return OutputStream::error(wafer_block::WaferError::new(
                    wafer_block::common::ErrorCode::InvalidArgument,
                    format!("invalid {} request: {}", $op_name, e.message),
                ))
            }
        }
    };
}

pub(crate) use decode_or_err;

/// SEC-003: enforce that the caller-supplied `wrap.resource` meta matches the
/// expected resource value from the decoded payload.
///
/// - Empty meta = legacy path (runtime already skipped WRAP); accept.
/// - Matching meta = accept.
/// - Mismatched meta = PERMISSION_DENIED, with `noun` naming the kind of
///   resource (e.g. `"key"`, `"collection"`, `"URL"`, `"resource"`) for
///   operator-friendly error messages.
pub fn check_wrap_resource(msg: &Message, expected: &str, noun: &str) -> Result<(), WaferError> {
    let supplied = msg.get_meta(META_WRAP_RESOURCE);
    if supplied.is_empty() || supplied == expected {
        Ok(())
    } else {
        Err(WaferError::new(
            ErrorCode::PermissionDenied,
            format!(
                "WRAP: wrap.resource meta '{supplied}' does not match payload {noun} '{expected}'"
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use wafer_block::Message;

    use super::*;

    fn msg_with_resource(resource: &str) -> Message {
        let mut m = Message::new("test.op");
        if !resource.is_empty() {
            m.set_meta(META_WRAP_RESOURCE, resource);
        }
        m
    }

    #[test]
    fn empty_meta_accepts_any_expected() {
        // Empty META_WRAP_RESOURCE = legacy path, always accept.
        let msg = msg_with_resource("");
        assert!(check_wrap_resource(&msg, "some-value", "key").is_ok());
    }

    #[test]
    fn matching_meta_accepts() {
        let msg = msg_with_resource("my-key");
        assert!(check_wrap_resource(&msg, "my-key", "key").is_ok());
    }

    #[test]
    fn mismatched_meta_returns_permission_denied_with_noun() {
        let msg = msg_with_resource("decoy-key");
        let err = check_wrap_resource(&msg, "real-key", "key").expect_err("mismatch must error");
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert!(
            err.message.contains("key"),
            "error message should contain the noun 'key', got: {}",
            err.message
        );
        assert!(
            err.message.contains("decoy-key"),
            "error message should contain the supplied value, got: {}",
            err.message
        );
        assert!(
            err.message.contains("real-key"),
            "error message should contain the expected value, got: {}",
            err.message
        );
    }

    #[test]
    fn noun_appears_in_error_message() {
        let msg = msg_with_resource("wrong-collection");
        let err =
            check_wrap_resource(&msg, "right-collection", "collection").expect_err("must error");
        assert!(
            err.message.contains("collection"),
            "noun 'collection' should appear in message: {}",
            err.message
        );
    }
}
