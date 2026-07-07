//! Shared handler utilities used across service block handlers.
//!
//! Provides `to_output` for serializing response payloads (MessagePack via codec)
//! and `decode_or_err!` for deserializing request bodies with uniform error
//! handling.

use wafer_block::{
    codec, common::ErrorCode, context::Context, streams::output::OutputStream, types::ResourceType,
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

/// Decode a request body via the codec AND authorize the caller for the
/// resource it targets, in one call. Returns the typed request only if
/// `ctx.check_resource_access` passed.
///
/// Bundling decode + authorize makes the WRAP check un-forgettable: an op
/// arm has no way to obtain its typed request without also running the
/// resource-access check, unlike `decode_or_err!` + a separate manual call
/// to `check_resource_access` (which a future op arm could simply omit).
///
/// Op arms should call this instead of the raw `decode_or_err!` macro
/// whenever the request targets a WRAP-governed resource.
///
/// - `resource` receives the decoded request and returns
///   `(resource_name, resource_type, is_write)`, which is passed straight to
///   `ctx.check_resource_access`.
/// - On decode failure, returns `Err(OutputStream::error(..))` with
///   `ErrorCode::InvalidArgument`, matching `decode_or_err!`'s message shape
///   exactly (`"invalid {op_name} request: {err}"`), and the resource
///   function is never invoked.
/// - On authorize failure, returns `Err(OutputStream::error(..))` wrapping
///   the `WaferError` from `check_resource_access` (typically
///   `PermissionDenied`).
pub fn decode_and_authorize<T>(
    ctx: &dyn Context,
    body: &[u8],
    op_name: &str,
    resource: impl FnOnce(&T) -> (String, ResourceType, bool),
) -> Result<T, OutputStream>
where
    T: serde::de::DeserializeOwned,
{
    let req = match codec::decode::<T>(body) {
        Ok(r) => r,
        Err(e) => {
            return Err(OutputStream::error(WaferError::new(
                ErrorCode::InvalidArgument,
                format!("invalid {op_name} request: {}", e.message),
            )))
        }
    };
    let (res, rt, is_write) = resource(&req);
    ctx.check_resource_access(&res, rt, is_write)
        .map_err(OutputStream::error)?;
    Ok(req)
}

#[cfg(test)]
mod decode_and_authorize_tests {
    use std::sync::Arc;

    use wafer_block::{
        streams::{input::InputStream, output::TerminalNotResponse},
        types::ResourceType,
        wafer_async_trait, Message,
    };

    use super::{codec, decode_and_authorize, Context, ErrorCode, OutputStream, WaferError};

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct TestReq {
        name: String,
        value: i32,
    }

    /// `Context` stub that always grants access.
    struct AllowCtx;

    #[wafer_async_trait]
    impl Context for AllowCtx {
        async fn call_block(
            &self,
            _block_name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            unimplemented!("not exercised by decode_and_authorize")
        }

        fn is_cancelled(&self) -> bool {
            unimplemented!("not exercised by decode_and_authorize")
        }

        fn config_get(&self, _key: &str) -> Option<&str> {
            unimplemented!("not exercised by decode_and_authorize")
        }

        fn clone_arc(&self) -> Arc<dyn Context> {
            unimplemented!("not exercised by decode_and_authorize")
        }

        fn check_resource_access(
            &self,
            _resource: &str,
            _resource_type: ResourceType,
            _is_write: bool,
        ) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// `Context` stub that always denies access, mirroring a real WRAP
    /// grant rejection.
    struct DenyCtx;

    #[wafer_async_trait]
    impl Context for DenyCtx {
        async fn call_block(
            &self,
            _block_name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            unimplemented!("not exercised by decode_and_authorize")
        }

        fn is_cancelled(&self) -> bool {
            unimplemented!("not exercised by decode_and_authorize")
        }

        fn config_get(&self, _key: &str) -> Option<&str> {
            unimplemented!("not exercised by decode_and_authorize")
        }

        fn clone_arc(&self) -> Arc<dyn Context> {
            unimplemented!("not exercised by decode_and_authorize")
        }

        fn check_resource_access(
            &self,
            _resource: &str,
            _resource_type: ResourceType,
            _is_write: bool,
        ) -> Result<(), WaferError> {
            Err(WaferError::new(
                ErrorCode::PermissionDenied,
                "denied by test ctx",
            ))
        }
    }

    async fn expect_error_code(out: OutputStream, expected: ErrorCode) -> WaferError {
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, expected, "unexpected error code: {}", e.message);
                e
            }
            other => panic!("expected {expected:?} error terminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn allow_ctx_returns_decoded_request() {
        let body = codec::encode(&TestReq {
            name: "widgets".into(),
            value: 7,
        })
        .expect("encode must succeed");

        let Ok(req) = decode_and_authorize::<TestReq>(&AllowCtx, &body, "test.op", |r| {
            (r.name.clone(), ResourceType::Db, false)
        }) else {
            panic!("allow ctx must pass the request through")
        };

        assert_eq!(
            req,
            TestReq {
                name: "widgets".into(),
                value: 7,
            }
        );
    }

    #[tokio::test]
    async fn deny_ctx_returns_permission_denied() {
        let body = codec::encode(&TestReq {
            name: "widgets".into(),
            value: 7,
        })
        .expect("encode must succeed");

        let out = decode_and_authorize::<TestReq>(&DenyCtx, &body, "test.op", |r| {
            (r.name.clone(), ResourceType::Db, false)
        })
        .expect_err("deny ctx must reject the request");

        expect_error_code(out, ErrorCode::PermissionDenied).await;
    }

    #[tokio::test]
    async fn malformed_body_errors_before_the_resource_closure_runs() {
        let body = b"not valid msgpack".to_vec();

        // If decode ever ran after (or without) gating on success, this
        // closure would run and the deliberate panic would fail the test.
        let out = decode_and_authorize::<TestReq>(&DenyCtx, &body, "test.op", |_req| {
            panic!("resource closure must not run when decode fails")
        })
        .expect_err("malformed body must error");

        let err = expect_error_code(out, ErrorCode::InvalidArgument).await;
        assert!(
            err.message.contains("invalid test.op request"),
            "decode error message should name the op, got: {}",
            err.message
        );
    }
}
