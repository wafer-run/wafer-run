//! Typed client for the network service.

use std::collections::HashMap;

use wafer_block::{
    codec,
    wire::network::{Request, Response, ResponseHeader},
    ErrorCode, Message, ServiceOp, WaferError,
};

use crate::stream::{CallStream, ResponseStream};

const BLOCK: &str = "wafer-run/network";

/// Buffered: fetch a URL and accumulate the full body into a `Vec<u8>`.
/// Returns the typed [`Response`].
pub fn do_request(
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> Result<Response, WaferError> {
    let mut response_stream = open_request_stream(method, url, headers, body)?;
    // First chunk is the header frame.
    let header_bytes = response_stream
        .next_chunk()?
        .ok_or_else(|| WaferError::new(ErrorCode::Internal, "stream ended before header frame"))?;
    let header: ResponseHeader = codec::decode(&header_bytes)?;

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
    let mut response_stream = open_request_stream(method, url, headers, body)?;
    let header_bytes = response_stream
        .next_chunk()?
        .ok_or_else(|| WaferError::new(ErrorCode::Internal, "stream ended before header frame"))?;
    let header: ResponseHeader = codec::decode(&header_bytes)?;
    Ok(NetworkResponseStream {
        inner: response_stream,
        header,
    })
}

/// Streaming response wrapper: exposes header metadata + chunked body access.
pub struct NetworkResponseStream {
    inner: ResponseStream,
    header: ResponseHeader,
}

impl NetworkResponseStream {
    pub fn status_code(&self) -> u16 {
        self.header.status_code
    }

    pub fn headers(&self) -> &HashMap<String, Vec<String>> {
        &self.header.headers
    }

    pub fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, WaferError> {
        self.inner.next_chunk()
    }
}

fn open_request_stream(
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> Result<ResponseStream, WaferError> {
    let req = Request {
        method: method.into(),
        url: url.into(),
        headers: headers.clone(),
        body: body.map(|b| b.to_vec()),
    };
    let req_bytes = codec::encode(&req)?;
    let msg = Message {
        kind: ServiceOp::NETWORK_DO_REQUEST.to_string(),
        meta: vec![],
    };
    let mut call = CallStream::open(BLOCK, &msg)?;
    call.write_chunk(&req_bytes)?;
    call.finish()
}
