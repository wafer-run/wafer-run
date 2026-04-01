use std::collections::HashMap;

// Re-export the trait and types from wafer-core.
pub use wafer_core::interfaces::network::service::{
    NetworkError, NetworkService, Request, Response,
};

// ---------------------------------------------------------------------------
// HTTP client concrete implementation (reqwest async)
// ---------------------------------------------------------------------------

/// Async reqwest-based network service for outbound HTTP calls.
/// The client is lazily initialized on first use.
pub struct HttpNetworkService {
    client: std::sync::OnceLock<reqwest::Client>,
}

impl HttpNetworkService {
    pub fn new() -> Self {
        Self {
            client: std::sync::OnceLock::new(),
        }
    }
}

impl Default for HttpNetworkService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl NetworkService for HttpNetworkService {
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
        // SSRF protection: block requests to private/internal IPs.
        // Disabled when ALLOW_PRIVATE_NETWORK=true (for local dev/testing).
        let allow_private = std::env::var("ALLOW_PRIVATE_NETWORK")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        if !allow_private && wafer_run::security::is_blocked_url(&req.url) {
            return Err(NetworkError::RequestError(
                "request to private/internal address is not allowed".to_string(),
            ));
        }

        let method = req
            .method
            .parse::<reqwest::Method>()
            .map_err(|e| NetworkError::RequestError(format!("invalid method: {}", e)))?;

        let client = self.client.get_or_init(|| {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        });
        let mut builder = client.request(method, &req.url);

        for (key, value) in &req.headers {
            builder = builder.header(key, value);
        }

        if let Some(ref body) = req.body {
            builder = builder.body(body.clone());
        }

        let response = builder
            .send()
            .await
            .map_err(|e| NetworkError::RequestError(e.to_string()))?;

        let status_code = response.status().as_u16();

        let mut headers = HashMap::new();
        for (name, value) in response.headers() {
            let entry = headers.entry(name.to_string()).or_insert_with(Vec::new);
            if let Ok(v) = value.to_str() {
                entry.push(v.to_string());
            }
        }

        let body = response
            .bytes()
            .await
            .map_err(|e| NetworkError::RequestError(format!("reading body: {}", e)))?;

        Ok(Response {
            status_code,
            headers,
            body: body.to_vec(),
        })
    }
}
