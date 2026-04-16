use std::collections::HashMap;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum NetworkError {
    #[error("request error: {0}")]
    RequestError(String),
    #[error("{0}")]
    Other(String),
}

/// Request represents an outbound network request.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: Option<Vec<u8>>,
}

/// Response represents an outbound network response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status_code: u16,
    pub headers: HashMap<String, Vec<String>>,
    pub body: Vec<u8>,
}

/// Service provides outbound network connectivity.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait NetworkService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError>;
}
