//! Network service client — calls `wafer/network` block via `call-block`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::wafer::block_world::runtime;
use crate::wafer::block_world::types::{Action, Message};

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

// --- Internal request/response types ---

#[derive(Serialize)]
struct HttpRequestReq<'a> {
    method: &'a str,
    url: &'a str,
    headers: HashMap<&'a str, &'a str>,
    body: Option<&'a [u8]>,
}

#[derive(Deserialize)]
struct HttpResponseResp {
    status_code: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

// --- Helpers ---

fn make_msg(kind: &str, data: &impl Serialize) -> Message {
    Message {
        kind: kind.to_string(),
        data: serde_json::to_vec(data).unwrap_or_default(),
        meta: Vec::new(),
    }
}

fn call_network(msg: &Message) -> Result<Vec<u8>, NetworkError> {
    let result = runtime::call_block("@wafer/network", msg);
    match result.action {
        Action::Error => {
            let err_msg = result.error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown network error".to_string());
            let kind = if err_msg.contains("SSRF") || err_msg.contains("ssrf") {
                "permission_denied"
            } else {
                "internal"
            };
            Err(NetworkError { kind: kind.into(), message: err_msg })
        }
        _ => Ok(result.response.map(|r| r.data).unwrap_or_default()),
    }
}

// --- Public API ---

/// Execute an outbound HTTP request.
pub fn do_request(method: &str, url: &str, headers: &HashMap<String, String>, body: Option<&[u8]>) -> Result<Response, NetworkError> {
    let header_refs: HashMap<&str, &str> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let msg = make_msg("network.do", &HttpRequestReq {
        method,
        url,
        headers: header_refs,
        body,
    });
    let data = call_network(&msg)?;
    let resp: HttpResponseResp = serde_json::from_slice(&data).map_err(|e| NetworkError {
        kind: "internal".into(),
        message: format!("failed to parse response: {e}"),
    })?;
    Ok(Response {
        status_code: resp.status_code,
        headers: resp.headers,
        body: resp.body,
    })
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
