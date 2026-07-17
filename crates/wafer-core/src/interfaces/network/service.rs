use std::collections::HashMap;

use thiserror::Error;
use wafer_block::OutputStream;
use wafer_block_macro::wafer_async_trait;

/// Errors returned by [`NetworkService`] operations.
#[derive(Error, Debug)]
pub enum NetworkError {
    /// Transport-level failure while issuing the request.
    #[error("request error: {0}")]
    RequestError(String),
    /// Catch-all variant carrying an arbitrary backend message.
    #[error("{0}")]
    Other(String),
}

/// Request represents an outbound network request.
#[derive(Debug, Clone)]
pub struct Request {
    /// HTTP method (e.g. `GET`, `POST`).
    pub method: String,
    /// Absolute request URL.
    pub url: String,
    /// Request headers as a flat name → value map.
    pub headers: HashMap<String, String>,
    /// Optional request body.
    pub body: Option<Vec<u8>>,
}

/// Response represents an outbound network response.
#[derive(Debug, Clone)]
pub struct Response {
    /// HTTP status code returned by the upstream server.
    pub status_code: u16,
    /// Response headers; one entry per header name with all values preserved.
    pub headers: HashMap<String, Vec<String>>,
    /// Response body bytes.
    pub body: Vec<u8>,
}

/// Response head (status + headers) paired with a streaming body by
/// [`NetworkService::do_request_streaming`].
///
/// Mirrors [`Response`] without the buffered `body` field — the body arrives
/// separately as an [`OutputStream`] of chunks so the whole response never
/// needs to sit in memory at once.
#[derive(Debug, Clone)]
pub struct ResponseHead {
    /// HTTP status code returned by the upstream server.
    pub status_code: u16,
    /// Response headers; one entry per header name with all values preserved.
    pub headers: HashMap<String, Vec<String>>,
}

/// Service provides outbound network connectivity.
#[wafer_async_trait]
pub trait NetworkService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// Issue `req` and return the upstream response, or a transport error.
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError>;

    /// Streaming variant of [`do_request`](Self::do_request): issue `req` and
    /// return the [`ResponseHead`] plus the response body as an
    /// [`OutputStream`] of chunks, rather than a fully-buffered [`Response`].
    ///
    /// The default forwards to [`do_request`](Self::do_request) and wraps the
    /// buffered body as a single-chunk stream, so existing backends keep
    /// working unchanged. Backends whose HTTP client exposes a chunked
    /// response body (e.g. reqwest's `bytes_stream`) SHOULD override this to
    /// avoid buffering the whole response in memory. A body-read failure is
    /// surfaced as an `Error` terminal on the returned stream.
    async fn do_request_streaming(
        &self,
        req: &Request,
    ) -> Result<(ResponseHead, OutputStream), NetworkError> {
        let resp = self.do_request(req).await?;
        let head = ResponseHead {
            status_code: resp.status_code,
            headers: resp.headers,
        };
        Ok((head, OutputStream::respond(resp.body)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_error_display_matches_variant() {
        assert_eq!(
            NetworkError::RequestError("connection refused".into()).to_string(),
            "request error: connection refused"
        );
        // `Other` passes the message through verbatim (no prefix).
        assert_eq!(
            NetworkError::Other("backend boom".into()).to_string(),
            "backend boom"
        );
    }
}
