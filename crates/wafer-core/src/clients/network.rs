use std::collections::HashMap;
#[cfg(not(feature = "wasm-component"))]
use std::pin::Pin;
#[cfg(not(feature = "wasm-component"))]
use std::task::{Context as TaskContext, Poll};

#[cfg(not(feature = "wasm-component"))]
use futures::Stream;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::stream::StreamEvent;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::streams::output::OutputStream;
/// Buffered response from an outbound network request.
///
/// Re-exported from `wafer_block::wire::network::Response` so the native
/// client and the SDK share one wire-format type.
pub use wafer_block::wire::network::Response as NetworkResponse;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::{common::ErrorCode, wire::network::ResponseHeader};
use wafer_block::{common::ServiceOp, wire::network::Request, WaferError};

#[cfg(not(feature = "wasm-component"))]
use super::{buffered_header_and_body, call_service_streaming, read_header_frame};
#[cfg(feature = "wasm-component")]
use super::{call_service, decode};

const BLOCK: &str = "wafer-run/network";

// ===========================================================================
// Public API — native async
// ===========================================================================

/// Buffered: fetch a URL through `wafer-run/network` and return the full
/// response body in a [`NetworkResponse`].
///
/// The handler emits a two-frame response: a `ResponseHeader` chunk followed
/// by zero-or-more body chunks. This helper consumes both frames and
/// reassembles them into the convenience [`NetworkResponse`] struct.
#[cfg(not(feature = "wasm-component"))]
pub async fn do_request(
    ctx: &dyn Context,
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> Result<NetworkResponse, WaferError> {
    do_request_via(ctx, BLOCK, method, url, headers, body).await
}

/// Like [`do_request`], but routes through a specific block instead of
/// `wafer-run/network`.
///
/// This allows callers to route through an alternative block (e.g.
/// `my-org/dispatcher`) that accepts the same `network.do` message format.
#[cfg(not(feature = "wasm-component"))]
pub async fn do_request_via(
    ctx: &dyn Context,
    block: &str,
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> Result<NetworkResponse, WaferError> {
    let req = Request {
        method: method.into(),
        url: url.into(),
        headers: headers.clone(),
        body: body.map(|b| b.to_vec()),
    };
    let out = call_service_streaming(
        ctx,
        block,
        ServiceOp::NETWORK_DO_REQUEST,
        &req,
        Some(url),
        false,
        Some("network"),
    )
    .await?;
    let (header, body): (ResponseHeader, Vec<u8>) =
        buffered_header_and_body(out, "decoding network response").await?;
    Ok(NetworkResponse {
        status_code: header.status_code,
        headers: header.headers,
        body,
    })
}

/// Streaming: fetch a URL and return a [`NativeNetworkResponseStream`] whose
/// header is already decoded; the remaining stream yields body chunks.
///
/// Use this when the response body is large (e.g. media download) and the
/// caller wants to forward chunks as they arrive instead of buffering the
/// full body in memory.
#[cfg(not(feature = "wasm-component"))]
pub async fn do_request_stream(
    ctx: &dyn Context,
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> Result<NativeNetworkResponseStream, WaferError> {
    let req = Request {
        method: method.into(),
        url: url.into(),
        headers: headers.clone(),
        body: body.map(|b| b.to_vec()),
    };
    // Streaming download: hit the dedicated `network.do_streaming` op so the
    // response body is forwarded chunk-by-chunk from the backend's
    // `do_request_streaming` stream, rather than the backend buffering the
    // whole response and this client re-framing it. Same request shape,
    // resource, and (read) authorization as the buffered `do_request` above.
    let mut out = call_service_streaming(
        ctx,
        BLOCK,
        ServiceOp::NETWORK_DO_REQUEST_STREAMING,
        &req,
        Some(url),
        false,
        Some("network"),
    )
    .await?;
    let header: ResponseHeader = read_header_frame(&mut out, "decoding network response").await?;
    Ok(NativeNetworkResponseStream {
        inner: out,
        header,
        finished: false,
    })
}

/// Streaming response wrapper for [`do_request_stream`].
///
/// The underlying `OutputStream` has already had its header frame consumed
/// before this wrapper is returned, so calls to [`futures::Stream::poll_next`]
/// yield body bytes directly. Non-`Complete` terminal events are translated
/// to a single `Err` item, after which the stream returns `None`.
#[cfg(not(feature = "wasm-component"))]
#[must_use = "response stream must be consumed"]
pub struct NativeNetworkResponseStream {
    inner: OutputStream,
    header: ResponseHeader,
    finished: bool,
}

#[cfg(not(feature = "wasm-component"))]
impl NativeNetworkResponseStream {
    /// HTTP status code from the response header.
    pub fn status_code(&self) -> u16 {
        self.header.status_code
    }

    /// HTTP response headers (multi-valued, as received).
    pub fn headers(&self) -> &HashMap<String, Vec<String>> {
        &self.header.headers
    }
}

#[cfg(not(feature = "wasm-component"))]
impl Stream for NativeNetworkResponseStream {
    type Item = Result<Vec<u8>, WaferError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        if self.finished {
            return Poll::Ready(None);
        }
        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(WaferError::new(
                        ErrorCode::Internal,
                        "network response stream ended without terminal event",
                    ))));
                }
                Poll::Ready(Some(StreamEvent::Chunk(bytes))) => {
                    return Poll::Ready(Some(Ok(bytes)));
                }
                Poll::Ready(Some(StreamEvent::Meta(_))) => continue,
                Poll::Ready(Some(StreamEvent::Complete { .. })) => {
                    self.finished = true;
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(StreamEvent::Error(e))) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(*e)));
                }
                Poll::Ready(Some(StreamEvent::Drop)) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(WaferError::new(
                        ErrorCode::Internal,
                        "network handler returned Drop",
                    ))));
                }
                Poll::Ready(Some(StreamEvent::Continue(msg))) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(WaferError::new(
                        ErrorCode::Internal,
                        format!("network handler returned Continue (kind: {})", msg.kind),
                    ))));
                }
                Poll::Ready(Some(StreamEvent::Halt { .. })) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(WaferError::new(
                        ErrorCode::Internal,
                        "network handler returned Halt",
                    ))));
                }
            }
        }
    }
}

// ===========================================================================
// Public API — WASM sync
// ===========================================================================

/// Perform an outbound HTTP request and return the response in a
/// [`NetworkResponse`] (WASM sync variant).
#[cfg(feature = "wasm-component")]
pub fn do_request(
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> Result<NetworkResponse, WaferError> {
    let req = Request {
        method: method.into(),
        url: url.into(),
        headers: headers.clone(),
        body: body.map(|b| b.to_vec()),
    };
    let data = call_service(
        BLOCK,
        ServiceOp::NETWORK_DO_REQUEST,
        &req,
        Some(url),
        false,
        Some("network"),
    )?;
    decode(&data)
}
