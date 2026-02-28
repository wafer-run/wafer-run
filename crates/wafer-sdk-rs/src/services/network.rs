//! Network service client using WIT-generated imports.

use std::collections::HashMap;

use crate::wafer::block_world::network as wit;
use crate::wafer::block_world::types::MetaEntry;

/// An HTTP response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

/// Network error type.
#[derive(Debug, Clone)]
pub struct NetworkError {
    pub kind: String,
    pub message: String,
}

impl std::fmt::Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for NetworkError {}

fn convert_wit_error(e: wit::NetworkError) -> NetworkError {
    match e {
        wit::NetworkError::RequestError => NetworkError { kind: "internal".into(), message: "request failed".into() },
        wit::NetworkError::SsrfBlocked => NetworkError { kind: "permission_denied".into(), message: "SSRF blocked".into() },
        wit::NetworkError::Other => NetworkError { kind: "internal".into(), message: "network error".into() },
    }
}

/// Execute an outbound HTTP request.
pub fn do_request(method: &str, url: &str, headers: &HashMap<String, String>, body: Option<&[u8]>) -> Result<Response, NetworkError> {
    let req = wit::HttpRequest {
        method: method.to_string(),
        url: url.to_string(),
        headers: headers.iter().map(|(k, v)| MetaEntry { key: k.clone(), value: v.clone() }).collect(),
        body: body.map(|b| b.to_vec()),
    };
    wit::do_request(&req)
        .map(|resp| Response {
            status_code: resp.status_code,
            headers: resp.headers.into_iter().map(|e| (e.key, e.value)).collect(),
            body: resp.body,
        })
        .map_err(convert_wit_error)
}

/// Convenience: perform a GET request.
pub fn get(url: &str) -> Result<Response, NetworkError> {
    do_request("GET", url, &HashMap::new(), None)
}

/// Convenience: perform a POST request with a JSON body.
pub fn post_json<T: serde::Serialize>(url: &str, body: &T) -> Result<Response, NetworkError> {
    let data = serde_json::to_vec(body).unwrap_or_default();
    let mut headers = HashMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    do_request("POST", url, &headers, Some(&data))
}
