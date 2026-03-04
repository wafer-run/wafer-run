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

// ---------------------------------------------------------------------------
// HTTP client concrete implementation (reqwest)
// ---------------------------------------------------------------------------

/// Simple reqwest-based network service for outbound HTTP calls.
/// The client is lazily initialized on first use.
#[cfg(feature = "network")]
pub struct HttpNetworkService {
    client: std::sync::OnceLock<reqwest::blocking::Client>,
}

#[cfg(feature = "network")]
impl HttpNetworkService {
    pub fn new() -> Self {
        Self {
            client: std::sync::OnceLock::new(),
        }
    }
}

#[cfg(feature = "network")]
impl Default for HttpNetworkService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "network")]
impl NetworkService for HttpNetworkService {
    fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
        let method = req.method.parse::<reqwest::Method>().map_err(|e| {
            NetworkError::RequestError(format!("invalid method: {}", e))
        })?;

        let client = self.client.get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new())
        });
        let mut builder = client.request(method, &req.url);

        for (key, value) in &req.headers {
            builder = builder.header(key, value);
        }

        if let Some(ref body) = req.body {
            builder = builder.body(body.clone());
        }

        let response = builder.send().map_err(|e| {
            NetworkError::RequestError(e.to_string())
        })?;

        let status_code = response.status().as_u16();

        let mut headers = HashMap::new();
        for (name, value) in response.headers() {
            let entry = headers.entry(name.to_string()).or_insert_with(Vec::new);
            if let Ok(v) = value.to_str() {
                entry.push(v.to_string());
            }
        }

        let body = response.bytes().map_err(|e| {
            NetworkError::RequestError(format!("reading body: {}", e))
        })?;

        Ok(Response {
            status_code,
            headers,
            body: body.to_vec(),
        })
    }
}
