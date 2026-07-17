//! Shared message handler logic for the network block.

use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    types::ResourceType,
    wire::network::{Request as WireRequest, ResponseHeader},
    *,
};

use super::service::{NetworkError, NetworkService, Request, ResponseHead};
use crate::interfaces::handler_util::{decode_and_authorize, stream_with_header};

// --- Helpers ---

fn network_error_to_wafer(e: NetworkError) -> WaferError {
    match e {
        NetworkError::RequestError(msg) => WaferError::new(ErrorCode::Unavailable, msg),
        NetworkError::Other(msg) => WaferError::new(ErrorCode::Internal, msg),
    }
}

/// Handle a network message by delegating to the given service.
///
/// `ctx` is the trusted host-side authorization surface: `NETWORK_DO_REQUEST`
/// and `NETWORK_DO_REQUEST_STREAMING` both authorize via
/// [`decode_and_authorize`], which bundles the codec decode with a call to
/// `ctx.check_resource_access` so the arm cannot obtain its typed request
/// without also being checked. `is_write` is deliberately `false` for both —
/// outbound HTTP requests aren't a WRAP write in the resource sense, and
/// flipping it would regress read-only network grants. The streaming op uses
/// the identical `(url, Network, read)` authorization tuple as the buffered
/// op, so it can never be reached with a weaker grant.
///
/// SSRF protection is NOT included here — it is platform-specific.
/// Native callers should check `wafer_core::security::is_blocked_url` before
/// calling the service. CF Workers are sandboxed by the runtime.
///
/// Wire protocol: the request is a single MessagePack-encoded
/// [`wire::network::Request`]. The response is emitted as **two frames** on
/// the OutputStream — a [`wire::network::ResponseHeader`] chunk followed by a
/// body chunk. The body chunk is omitted entirely when the body is empty
/// (zero chunks → empty body on the consumer side). `NETWORK_DO_REQUEST`
/// buffers the whole body first; `NETWORK_DO_REQUEST_STREAMING` forwards the
/// service's `do_request_streaming` body chunks verbatim as they arrive,
/// under the same two-frame shape.
pub async fn handle_message(
    service: &dyn NetworkService,
    ctx: &dyn Context,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::NETWORK_DO_REQUEST => {
            let wire_req = match decode_and_authorize::<WireRequest>(ctx, body, "network.do", |r| {
                (r.url.clone(), ResourceType::Network, false)
            }) {
                Ok(r) => r,
                Err(out) => return out,
            };

            let request = Request {
                method: wire_req.method,
                url: wire_req.url,
                headers: wire_req.headers,
                body: wire_req.body,
            };

            match service.do_request(&request).await {
                Ok(resp) => {
                    let header = ResponseHeader {
                        status_code: resp.status_code,
                        headers: resp.headers,
                    };
                    let body_bytes = resp.body;
                    OutputStream::from_producer(|sink, _cancel| async move {
                        let header_bytes = match codec::encode(&header) {
                            Ok(b) => b,
                            Err(e) => {
                                let _ = sink
                                    .error(WaferError::new(
                                        ErrorCode::Internal,
                                        format!("encoding network response header: {e}"),
                                    ))
                                    .await;
                                return;
                            }
                        };
                        if sink.send_chunk(header_bytes).await.is_err() {
                            return;
                        }
                        // Body is the second frame. Skip the chunk entirely
                        // when empty — consumers reconstruct an empty body
                        // from zero chunks.
                        if !body_bytes.is_empty() && sink.send_chunk(body_bytes).await.is_err() {
                            return;
                        }
                        let _ = sink.complete(vec![]).await;
                    })
                }
                Err(e) => OutputStream::error(network_error_to_wafer(e)),
            }
        }
        ServiceOp::NETWORK_DO_REQUEST_STREAMING => {
            // Same request shape and WRAP authorization as `NETWORK_DO_REQUEST`
            // — a read of the target URL — so the streaming download can never
            // be reached with a weaker grant than the buffered request.
            let wire_req =
                match decode_and_authorize::<WireRequest>(ctx, body, "network.do_streaming", |r| {
                    (r.url.clone(), ResourceType::Network, false)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };

            let request = Request {
                method: wire_req.method,
                url: wire_req.url,
                headers: wire_req.headers,
                body: wire_req.body,
            };

            match service.do_request_streaming(&request).await {
                Ok((head, body_stream)) => {
                    // Two-frame response: a `ResponseHeader` (status + headers)
                    // header chunk followed by the body forwarded verbatim from
                    // the service's stream (never collapsed via
                    // `collect_buffered`).
                    let ResponseHead {
                        status_code,
                        headers,
                    } = head;
                    let header = ResponseHeader {
                        status_code,
                        headers,
                    };
                    stream_with_header(header, body_stream, "network.do_streaming")
                }
                Err(e) => OutputStream::error(network_error_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("unknown network operation: {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use futures::StreamExt;
    use wafer_block::wire::network::Request as WireRequest;

    use super::*;
    use crate::interfaces::network::service::Response;

    /// `Context` stub that grants every resource-access check — these tests
    /// exercise the streaming wire-protocol shape, not authorization.
    struct AllowCtx;

    #[wafer_block::wafer_async_trait]
    impl Context for AllowCtx {
        async fn call_block(
            &self,
            _block_name: &str,
            _msg: Message,
            _input: wafer_block::streams::input::InputStream,
        ) -> OutputStream {
            unimplemented!("not exercised by these tests")
        }

        fn is_cancelled(&self) -> bool {
            unimplemented!("not exercised by these tests")
        }

        fn config_get(&self, _key: &str) -> Option<&str> {
            unimplemented!("not exercised by these tests")
        }

        fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
            unimplemented!("not exercised by these tests")
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

    /// A `NetworkService` that returns a fixed-body 200 response.
    struct StubNet {
        body: Vec<u8>,
    }

    #[async_trait::async_trait]
    impl NetworkService for StubNet {
        async fn do_request(&self, _req: &Request) -> Result<Response, NetworkError> {
            Ok(Response {
                status_code: 200,
                headers: HashMap::new(),
                body: self.body.clone(),
            })
        }
    }

    fn do_request_body(url: &str) -> Vec<u8> {
        codec::encode(&WireRequest {
            method: "GET".to_string(),
            url: url.to_string(),
            headers: HashMap::new(),
            body: None,
        })
        .expect("encode wire request")
    }

    #[test]
    fn network_error_maps_to_unavailable_and_internal() {
        assert_eq!(
            network_error_to_wafer(NetworkError::RequestError("timeout".into())).code,
            ErrorCode::Unavailable,
        );
        assert_eq!(
            network_error_to_wafer(NetworkError::Other("boom".into())).code,
            ErrorCode::Internal,
        );
    }

    #[tokio::test]
    async fn empty_body_yields_single_header_frame() {
        let svc = StubNet { body: Vec::new() };
        let msg = Message::new(ServiceOp::NETWORK_DO_REQUEST);
        let out = handle_message(
            &svc,
            &AllowCtx,
            &msg,
            &do_request_body("http://example.com"),
        )
        .await;
        let chunks: Vec<Vec<u8>> = out.body_stream().collect().await;
        assert_eq!(
            chunks.len(),
            1,
            "an empty response body must emit the header frame only (zero body chunks)"
        );
    }

    #[tokio::test]
    async fn non_empty_body_yields_header_and_body_frames() {
        let svc = StubNet {
            body: b"hello".to_vec(),
        };
        let msg = Message::new(ServiceOp::NETWORK_DO_REQUEST);
        let out = handle_message(
            &svc,
            &AllowCtx,
            &msg,
            &do_request_body("http://example.com"),
        )
        .await;
        let chunks: Vec<Vec<u8>> = out.body_stream().collect().await;
        assert_eq!(
            chunks.len(),
            2,
            "a non-empty body must emit header + body frames"
        );
        assert_eq!(
            chunks[1], b"hello",
            "the second frame carries the body bytes"
        );
    }

    #[tokio::test]
    async fn unknown_operation_is_unimplemented() {
        let svc = StubNet { body: Vec::new() };
        let msg = Message::new("network.bogus");
        let out = handle_message(&svc, &AllowCtx, &msg, &[]).await;
        match out.collect_buffered().await {
            Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Unimplemented);
            }
            other => panic!("expected Unimplemented error terminal, got {other:?}"),
        }
    }
}
