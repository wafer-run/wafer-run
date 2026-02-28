use std::collections::HashMap;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum NetworkError {
    #[error("request error: {0}")]
    RequestError(String),
    #[error("{0}")]
    Other(String),
}

/// Service provides outbound network connectivity.
pub trait NetworkService: Send + Sync {
    fn do_request(&self, req: &Request) -> Result<Response, NetworkError>;
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
