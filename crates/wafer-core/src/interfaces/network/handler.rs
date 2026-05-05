//! Shared message handler logic for the network block.

use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    wire::network::{Request as WireRequest, ResponseHeader},
    *,
};

use super::service::{NetworkError, NetworkService, Request};

// --- Helpers ---

fn network_error_to_wafer(e: NetworkError) -> WaferError {
    match e {
        NetworkError::RequestError(msg) => WaferError::new(ErrorCode::UNAVAILABLE, msg),
        NetworkError::Other(msg) => WaferError::new(ErrorCode::INTERNAL, msg),
    }
}

/// Handle a network message by delegating to the given service.
///
/// SSRF protection is NOT included here — it is platform-specific.
/// Native callers should check `wafer_run::security::is_blocked_url` before
/// calling the service. CF Workers are sandboxed by the runtime.
///
/// Wire protocol: the request is a single MessagePack-encoded
/// [`wire::network::Request`]. The response is emitted as **two frames** on
/// the OutputStream — a [`wire::network::ResponseHeader`] chunk followed by a
/// body chunk. The body chunk is omitted entirely when the body is empty
/// (zero chunks → empty body on the consumer side).
pub async fn handle_message(
    service: &dyn NetworkService,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::NETWORK_DO_REQUEST => {
            let wire_req: WireRequest = match codec::decode(body) {
                Ok(r) => r,
                Err(e) => {
                    return OutputStream::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid network.do request: {e}"),
                    ))
                }
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
                                        ErrorCode::INTERNAL,
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
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown network operation: {other}"),
        )),
    }
}
