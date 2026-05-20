use std::collections::HashMap;

use thiserror::Error;
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

/// Service provides outbound network connectivity.
#[wafer_async_trait]
pub trait NetworkService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// Issue `req` and return the upstream response, or a transport error.
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError>;
}
