//! Shared handler utilities used across service block handlers.
//!
//! Provides `to_output` for serializing response payloads (MessagePack via codec)
//! and `decode_or_err!` for deserializing request bodies with uniform error
//! handling.

use wafer_block::{
    codec,
    common::ErrorCode,
    context::Context,
    stream::{self, StreamEvent},
    streams::output::OutputStream,
    types::ResourceType,
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

/// Emit a two-frame streaming service response: a codec-encoded `header` frame,
/// a [`wafer_block::stream::raw_frames_marker`] `Meta` event, then the body
/// forwarded **verbatim** from `body`.
///
/// The marker is what tells a consumer that re-encodes frames (the wasmi codec
/// bridge, for a guest on a non-MessagePack host codec) that the header is a
/// DTO but everything after it is opaque application bytes. Consumers that just
/// concatenate the body chunks skip `Meta` events already.
///
/// This is the streaming counterpart to the buffered two-frame path (a header
/// chunk followed by a single buffered body chunk). Instead of buffering the
/// whole body, it forwards each [`StreamEvent::Chunk`] from the service's
/// [`OutputStream`] as it arrives, preserving frame boundaries and
/// back-pressure end to end — the object/response never sits in memory whole.
///
/// Terminal propagation: the body's `Complete` becomes this stream's
/// `Complete` (carrying the body's trailing meta) and its `Error` becomes this
/// stream's `Error`. Because the header chunk has already been sent, a
/// body-free terminal (`Drop`/`Continue`/`Halt`) or an abrupt end without a
/// terminal is a protocol violation for a body producer and is surfaced as an
/// `Internal` error terminal. If the downstream consumer drops the stream, the
/// paired `CancellationToken` aborts a blocked upstream read promptly (the
/// `body` stream is dropped, cancelling *its* producer in turn).
///
/// `context` labels any error message so a failure can be attributed to a
/// specific op (e.g. `"storage.get_streaming"`).
pub fn stream_with_header<H>(header: H, body: OutputStream, context: &'static str) -> OutputStream
where
    H: serde::Serialize + Send + 'static,
{
    use futures::StreamExt;

    OutputStream::from_producer(move |sink, cancel| async move {
        let header_bytes = match codec::encode(&header) {
            Ok(b) => b,
            Err(e) => {
                let _ = sink
                    .error(WaferError::new(
                        ErrorCode::Internal,
                        format!("{context}: encoding header frame: {}", e.message),
                    ))
                    .await;
                return;
            }
        };
        if sink.send_chunk(header_bytes).await.is_err() {
            // Consumer already dropped — nothing more to do.
            return;
        }
        // Everything after this marker is body: raw bytes, not a wire DTO.
        if sink.send_meta(stream::raw_frames_marker()).await.is_err() {
            return;
        }

        let mut body = body;
        loop {
            // Race the body against cancellation so a dropped consumer aborts a
            // blocked upstream read promptly rather than after the next chunk.
            // `run_until_cancelled` lives on `tokio_util` (an all-target dep),
            // so this stays valid on the wasm-component build where `tokio`
            // (and `tokio::select!`) is not linked.
            let evt = match cancel.run_until_cancelled(body.next()).await {
                // Consumer dropped the stream — stop and drop `body`, which
                // cancels the service's producer in turn.
                None => return,
                Some(None) => {
                    let _ = sink
                        .error(WaferError::new(
                            ErrorCode::Internal,
                            format!("{context}: body stream ended without a terminal event"),
                        ))
                        .await;
                    return;
                }
                Some(Some(evt)) => evt,
            };
            match evt {
                StreamEvent::Chunk(bytes) => {
                    if sink.send_chunk(bytes).await.is_err() {
                        return;
                    }
                }
                StreamEvent::Meta(entry) => {
                    if sink.send_meta(entry).await.is_err() {
                        return;
                    }
                }
                StreamEvent::Complete { meta } => {
                    let _ = sink.complete(meta).await;
                    return;
                }
                StreamEvent::Error(e) => {
                    let _ = sink.error(*e).await;
                    return;
                }
                StreamEvent::Drop | StreamEvent::Continue(_) | StreamEvent::Halt { .. } => {
                    let _ = sink
                        .error(WaferError::new(
                            ErrorCode::Internal,
                            format!(
                                "{context}: body producer emitted a non-body terminal after the header frame"
                            ),
                        ))
                        .await;
                    return;
                }
            }
        }
    })
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

#[cfg(test)]
mod stream_with_header_tests {
    use futures::StreamExt;
    use wafer_block::{
        codec,
        common::ErrorCode,
        stream::{self, StreamEvent},
        streams::output::{OutputStream, TerminalNotResponse},
        WaferError,
    };

    use super::stream_with_header;

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Header {
        n: u32,
    }

    fn chunk_payloads(events: &[StreamEvent]) -> Vec<Vec<u8>> {
        events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Chunk(b) => Some(b.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn forwards_header_then_body_chunks_verbatim_then_complete() {
        let body = OutputStream::from_producer(|sink, _cancel| async move {
            sink.send_chunk(b"a".to_vec()).await.ok();
            sink.send_chunk(b"b".to_vec()).await.ok();
            sink.complete(vec![]).await.ok();
        });
        let out = stream_with_header(Header { n: 7 }, body, "test.op");
        let events: Vec<StreamEvent> = out.collect().await;

        let chunks = chunk_payloads(&events);
        assert_eq!(chunks.len(), 3, "header frame + two verbatim body frames");
        let header: Header = codec::decode(&chunks[0]).expect("header frame decodes");
        assert_eq!(header, Header { n: 7 });
        assert_eq!(chunks[1], b"a");
        assert_eq!(chunks[2], b"b");
        assert!(
            matches!(events.last(), Some(StreamEvent::Complete { .. })),
            "must terminate with the body's Complete"
        );
    }

    /// The raw-frame marker sits between the header frame and the first body
    /// frame — a consumer that re-encodes frames must transcode the header and
    /// forward everything after the marker verbatim, with no sniffing.
    #[tokio::test]
    async fn marks_the_frames_after_the_header_as_raw() {
        let body = OutputStream::from_producer(|sink, _cancel| async move {
            sink.send_chunk(b"a".to_vec()).await.ok();
            sink.complete(vec![]).await.ok();
        });
        let events: Vec<StreamEvent> = stream_with_header(Header { n: 7 }, body, "test.op")
            .collect()
            .await;

        assert!(
            matches!(events.first(), Some(StreamEvent::Chunk(_))),
            "frame 0 is the encoded header, got: {:?}",
            events.first()
        );
        assert_eq!(
            events.get(1),
            Some(&StreamEvent::Meta(stream::raw_frames_marker())),
            "the marker must precede the first body frame"
        );
        assert_eq!(events.get(2), Some(&StreamEvent::Chunk(b"a".to_vec())));
        assert_eq!(
            events
                .iter()
                .filter(
                    |e| matches!(e, StreamEvent::Meta(m) if m.key == stream::FRAME_ENCODING_META)
                )
                .count(),
            1,
            "exactly one marker per stream"
        );
    }

    #[tokio::test]
    async fn propagates_body_error_terminal_after_partial_chunks() {
        let body = OutputStream::from_producer(|sink, _cancel| async move {
            sink.send_chunk(b"partial".to_vec()).await.ok();
            let _ = sink
                .error(WaferError::new(ErrorCode::Unavailable, "upstream boom"))
                .await;
        });
        let out = stream_with_header(Header { n: 1 }, body, "test.op");
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Unavailable);
                assert_eq!(e.message, "upstream boom");
            }
            other => panic!("expected the body's Error terminal to propagate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_body_terminal_after_header_becomes_internal_error() {
        // A body that yields only a `Drop` terminal (no chunks). Once the
        // header frame has been sent, a body-free terminal is a protocol
        // violation and must surface as an Internal error, not a silent close.
        let out = stream_with_header(
            Header { n: 0 },
            OutputStream::drop_request(),
            "storage.get_streaming",
        );
        let mut out = out;

        let first = out.next().await.expect("header frame");
        assert!(
            matches!(first, StreamEvent::Chunk(_)),
            "the header frame is always emitted first"
        );
        let marker = out.next().await.expect("raw-frame marker");
        assert_eq!(
            marker,
            StreamEvent::Meta(stream::raw_frames_marker()),
            "the raw-frame marker follows the header, before any body event"
        );
        match out.next().await.expect("terminal event") {
            StreamEvent::Error(e) => {
                assert_eq!(e.code, ErrorCode::Internal);
                assert!(
                    e.message.contains("storage.get_streaming"),
                    "error must name the op context, got: {}",
                    e.message
                );
            }
            other => panic!("expected an Internal error terminal, got {other:?}"),
        }
    }
}
