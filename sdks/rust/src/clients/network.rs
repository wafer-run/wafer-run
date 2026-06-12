//! Typed client for the network service.
//!
//! `NETWORK_DO_REQUEST` uses a header-then-body response protocol: the
//! first frame carries the encoded [`ResponseHeader`], subsequent frames
//! carry raw body bytes. [`do_request`] accumulates the body;
//! [`do_request_stream`] returns a [`NetworkResponseStream`] for chunked
//! access.

use std::collections::HashMap;

use wafer_block::{
    codec,
    wire::network::{Request, Response, ResponseHeader},
    ErrorCode, ServiceOp, WaferError,
};

use super::common::{decode_frame, open_buffered};
use crate::stream::ResponseStream;

const BLOCK: &str = "wafer-run/network";

/// Buffered: fetch a URL and accumulate the full body into a `Vec<u8>`.
/// Returns the typed [`Response`].
pub fn do_request(
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> Result<Response, WaferError> {
    let (mut response_stream, header) = open_with_header(method, url, headers, body)?;

    // Subsequent chunks are body bytes; accumulate.
    let mut full_body = Vec::new();
    while let Some(chunk) = response_stream.next_chunk()? {
        full_body.extend(chunk);
    }

    Ok(Response {
        status_code: header.status_code,
        headers: header.headers,
        body: full_body,
    })
}

/// Streaming: fetch a URL, returning chunks as they arrive.
pub fn do_request_stream(
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> Result<NetworkResponseStream, WaferError> {
    let (response_stream, header) = open_with_header(method, url, headers, body)?;
    Ok(NetworkResponseStream {
        inner: response_stream,
        header,
    })
}

/// Streaming response wrapper: exposes header metadata + chunked body access.
///
/// The response header has already been consumed from the underlying stream
/// before this wrapper is returned, so calls to [`Self::next_chunk`] yield
/// body bytes directly.
#[must_use = "response stream must be consumed via next_chunk"]
pub struct NetworkResponseStream {
    inner: ResponseStream,
    header: ResponseHeader,
}

impl NetworkResponseStream {
    /// HTTP status code from the response header.
    pub fn status_code(&self) -> u16 {
        self.header.status_code
    }

    /// HTTP response headers (multi-valued, as received).
    pub fn headers(&self) -> &HashMap<String, Vec<String>> {
        &self.header.headers
    }

    /// Pull the next body chunk from the stream.
    ///
    /// Returns `Ok(Some(bytes))` for each body chunk, and `Ok(None)` once the
    /// body has been fully delivered (end-of-stream). The response header was
    /// already consumed before this wrapper was constructed, so callers do not
    /// need to skip a header chunk — every chunk yielded here is body bytes.
    pub fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, WaferError> {
        self.inner.next_chunk()
    }
}

/// Open a `NETWORK_DO_REQUEST` stream and consume its leading
/// [`ResponseHeader`] frame. Shared by [`do_request`] and
/// [`do_request_stream`].
fn open_with_header(
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> Result<(ResponseStream, ResponseHeader), WaferError> {
    let req = Request {
        method: method.into(),
        url: url.into(),
        headers: headers.clone(),
        body: body.map(|b| b.to_vec()),
    };
    let req_bytes = codec::encode(&req)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::NETWORK_DO_REQUEST, &req_bytes)?;
    let header_bytes = response_stream
        .next_chunk()?
        .ok_or_else(|| WaferError::new(ErrorCode::Internal, "stream ended before header frame"))?;
    let header = decode_frame(&header_bytes, "network response header")?;
    Ok((response_stream, header))
}
