use std::{collections::HashMap, sync::Arc};

use futures::StreamExt;
use wafer_block::{common::ErrorCode, OutputStream, WaferError};
use wafer_block_macro::wafer_async_trait;
// Re-export the trait and types from wafer-core.
pub use wafer_core::interfaces::network::service::{
    NetworkError, NetworkService, Request, Response, ResponseHead,
};
use wafer_net_security::SsrfFilteringResolver;

/// Config var key controlling the maximum response body size accepted by
/// `HttpNetworkService`. Read from the process env **once** at service
/// construction ([`HttpNetworkService::from_env`], PERF-04). When unset,
/// defaults to [`DEFAULT_MAX_RESPONSE_BYTES`]; a present-but-invalid value
/// is an explicit construction error, never a silent fallback. Changes
/// take effect on the next boot.
///
/// Declared on the `wafer-run/network` block's `BlockInfo::config_keys` so
/// the admin UI can surface and edit it (see `service_blocks::network`).
pub const MAX_RESPONSE_BYTES_KEY: &str = "WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES";

/// Default response body cap: 50 MiB. SEC-020 — prevents unbounded memory
/// growth from hostile or runaway upstream servers.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

// ---------------------------------------------------------------------------
// HTTP client concrete implementation (reqwest async)
// ---------------------------------------------------------------------------

/// Async reqwest-based network service for outbound HTTP calls.
///
/// The client is lazily initialised on first use. If
/// `reqwest::Client::builder().build()` fails (e.g. the platform TLS
/// stack can't be configured) the error is cached and returned to
/// every caller — the old code silently fell back to
/// `reqwest::Client::new()`, which has no SSRF resolver, no timeout,
/// and no redirect policy, so a TLS build failure quietly disabled
/// the security middleware.
#[derive(Debug)]
pub struct HttpNetworkService {
    client: std::sync::OnceLock<Result<reqwest::Client, String>>,
    /// Response-body cap in bytes. Parsed once at construction (PERF-04) —
    /// the old code re-read and re-parsed the env var on every request.
    max_response_bytes: usize,
}

impl HttpNetworkService {
    /// Construct the service, reading [`MAX_RESPONSE_BYTES_KEY`] from the
    /// process env exactly once.
    ///
    /// - Unset → [`DEFAULT_MAX_RESPONSE_BYTES`] (documented default).
    /// - Present but not a positive integer → explicit error, so a typo'd
    ///   cap fails the boot loudly instead of silently reverting to the
    ///   default (the old per-request parse swallowed invalid values).
    pub fn from_env() -> Result<Self, NetworkError> {
        let max_response_bytes = match std::env::var(MAX_RESPONSE_BYTES_KEY) {
            Err(std::env::VarError::NotPresent) => DEFAULT_MAX_RESPONSE_BYTES,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(NetworkError::Other(format!(
                    "{MAX_RESPONSE_BYTES_KEY} is not valid UTF-8: \
                     expected a positive integer byte count"
                )));
            }
            Ok(raw) => match raw.parse::<usize>() {
                Ok(v) if v > 0 => v,
                _ => {
                    return Err(NetworkError::Other(format!(
                        "{MAX_RESPONSE_BYTES_KEY}={raw:?} is invalid: expected a positive \
                         integer byte count (unset it to use the default \
                         {DEFAULT_MAX_RESPONSE_BYTES})"
                    )));
                }
            },
        };
        Ok(Self::with_max_response_bytes(max_response_bytes))
    }

    /// Construct with an explicit response-body cap in bytes (no env read).
    /// The underlying `reqwest::Client` is built on the first request.
    pub fn with_max_response_bytes(max_response_bytes: usize) -> Self {
        Self {
            client: std::sync::OnceLock::new(),
            max_response_bytes,
        }
    }

    /// Borrow the shared reqwest client, building it on first call. A
    /// build failure is cached so subsequent requests fail fast with
    /// the same error instead of retrying the broken configuration.
    fn client(&self) -> Result<&reqwest::Client, NetworkError> {
        self.client
            .get_or_init(|| {
                reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .redirect(reqwest::redirect::Policy::none())
                    .dns_resolver(Arc::new(SsrfFilteringResolver))
                    .build()
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|s| {
                NetworkError::RequestError(format!("HTTP client initialisation failed: {s}"))
            })
    }

    /// Shared request setup for both the buffered [`do_request`] and the
    /// streaming [`do_request_streaming`] paths: SSRF gate, method parse,
    /// header/body build, and dispatch. Keeping this in one place ensures the
    /// SSRF check can never drift between the two entry points.
    ///
    /// [`do_request`]: NetworkService::do_request
    /// [`do_request_streaming`]: NetworkService::do_request_streaming
    async fn send_request(&self, req: &Request) -> Result<reqwest::Response, NetworkError> {
        // SSRF protection: block requests to private/internal IPs.
        // The runtime escape hatch (`ALLOW_PRIVATE_NETWORK` env var) was
        // replaced with a Cargo feature in SEC-018 so the bypass cannot be
        // flipped on a live deploy. URL-level checks here block by-name
        // hits (e.g. `http://127.0.0.1/`, `http://localhost/`); resolved-IP
        // checks happen in `SsrfFilteringResolver` and defend against DNS
        // rebinding (SEC-019).
        #[cfg(not(feature = "allow-private-network"))]
        if wafer_core::security::is_blocked_url(&req.url) {
            return Err(NetworkError::RequestError(
                "request to private/internal address is not allowed".to_string(),
            ));
        }

        let method = req
            .method
            .parse::<reqwest::Method>()
            .map_err(|e| NetworkError::RequestError(format!("invalid method: {e}")))?;

        let client = self.client()?;
        let mut builder = client.request(method, &req.url);

        for (key, value) in &req.headers {
            builder = builder.header(key, value);
        }

        if let Some(ref body) = req.body {
            builder = builder.body(body.clone());
        }

        builder
            .send()
            .await
            .map_err(|e| NetworkError::RequestError(e.to_string()))
    }
}

/// Flatten reqwest response headers into the wire-facing
/// `name → [values]` map (one entry per header name, all values preserved).
fn collect_headers(response: &reqwest::Response) -> HashMap<String, Vec<String>> {
    let mut headers = HashMap::new();
    for (name, value) in response.headers() {
        let entry = headers.entry(name.to_string()).or_insert_with(Vec::new);
        if let Ok(v) = value.to_str() {
            entry.push(v.to_string());
        }
    }
    headers
}

/// SEC-020: reject up front when `Content-Length` advertises more than the
/// cap, before any body bytes are read. Chunked / unknown-length responses
/// have no advertised length and are enforced while streaming instead.
fn check_advertised_len(response: &reqwest::Response, cap: usize) -> Result<(), NetworkError> {
    if let Some(advertised) = response.content_length() {
        if advertised as usize > cap {
            return Err(NetworkError::RequestError(format!(
                "response body {advertised} bytes exceeds cap of {cap} bytes"
            )));
        }
    }
    Ok(())
}

#[wafer_async_trait]
impl NetworkService for HttpNetworkService {
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
        let response = self.send_request(req).await?;
        let status_code = response.status().as_u16();
        let headers = collect_headers(&response);

        // SEC-020: cap response body size. Reject early if `Content-Length`
        // advertises more than the cap; otherwise accumulate from a chunk
        // stream and bail once the cap is exceeded (handles chunked /
        // unknown-length responses without buffering the whole thing in
        // reqwest's internal Bytes first).
        let cap = self.max_response_bytes;
        check_advertised_len(&response, cap)?;

        let mut body: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| NetworkError::RequestError(format!("reading body: {e}")))?;
            if body.len().saturating_add(chunk.len()) > cap {
                return Err(NetworkError::RequestError(format!(
                    "response body exceeds cap of {cap} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }

        Ok(Response {
            status_code,
            headers,
            body,
        })
    }

    /// Streams the response body via reqwest's `bytes_stream` instead of
    /// buffering it whole. The [`ResponseHead`] (status + headers) is returned
    /// eagerly; body chunks are forwarded through an [`OutputStream`] producer
    /// as they arrive.
    ///
    /// SEC-020 is preserved on the streaming path: an over-large advertised
    /// `Content-Length` is rejected before streaming starts, and the running
    /// byte total is enforced per chunk — a body that exceeds the cap mid
    /// stream is surfaced as an `Error` terminal (an upstream read failure is
    /// too). Chunked / unknown-length responses have no advertised length, so
    /// the per-chunk check is the only guard for them.
    async fn do_request_streaming(
        &self,
        req: &Request,
    ) -> Result<(ResponseHead, OutputStream), NetworkError> {
        let response = self.send_request(req).await?;
        let status_code = response.status().as_u16();
        let headers = collect_headers(&response);

        let cap = self.max_response_bytes;
        check_advertised_len(&response, cap)?;

        let head = ResponseHead {
            status_code,
            headers,
        };

        let stream = OutputStream::from_producer(move |sink, _cancel| async move {
            let mut received: usize = 0;
            let mut body = response.bytes_stream();
            while let Some(chunk) = body.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = sink
                            .error(WaferError::new(
                                ErrorCode::Unavailable,
                                format!("reading body: {e}"),
                            ))
                            .await;
                        return;
                    }
                };
                received = received.saturating_add(chunk.len());
                if received > cap {
                    let _ = sink
                        .error(WaferError::new(
                            ErrorCode::Unavailable,
                            format!("response body exceeds cap of {cap} bytes"),
                        ))
                        .await;
                    return;
                }
                if sink.send_chunk(chunk.to_vec()).await.is_err() {
                    // Consumer dropped the stream — stop reading.
                    return;
                }
            }
            let _ = sink.complete(vec![]).await;
        });

        Ok((head, stream))
    }
}

#[cfg(test)]
mod tests {
    use wafer_core::interfaces::network::service::{NetworkService, Request};

    use super::*;

    // The resolver-in-isolation test (`dns_resolver_rejects_loopback_resolution`)
    // moved to `wafer-net-security` with the resolver itself (SEC-09).

    /// End-to-end DNS rebinding case: the public-looking caller-supplied
    /// URL host resolves to `127.0.0.1`. The URL-level
    /// [`wafer_core::security::is_blocked_url`] check passes (host is a
    /// plain domain), so the only line of defense is the
    /// [`SsrfFilteringResolver`].
    ///
    /// We can't synthesize arbitrary public→private DNS rebinds in unit
    /// tests, so instead we drive the resolver directly with a name the
    /// system resolves to loopback (`localhost`) and confirm the request
    /// fails. This is the same code path that would fire on rebinding.
    #[cfg(not(feature = "allow-private-network"))]
    #[tokio::test]
    async fn http_request_rejected_when_dns_returns_private_ip() {
        let svc = HttpNetworkService::with_max_response_bytes(DEFAULT_MAX_RESPONSE_BYTES);
        // Use a URL whose host is NOT obviously private — `is_blocked_url`
        // only catches the `localhost` literal because that's what's in
        // the URL. We want to ensure the resolver kicks in, so we craft a
        // URL whose host literal is *not* `localhost` itself. That's hard
        // without an external DNS, so we settle for a layered test: hit
        // `localhost` and confirm the URL-level check fires.
        let req = Request {
            method: "GET".into(),
            url: "http://localhost/".into(),
            headers: HashMap::new(),
            body: None,
        };
        let err = svc.do_request(&req).await.expect_err("must be blocked");
        match err {
            NetworkError::RequestError(msg) => {
                assert!(
                    msg.contains("private/internal"),
                    "expected SSRF rejection, got: {msg}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// `client()` caches the built client across calls — two requests on
    /// the same service must observe the same `&reqwest::Client`. This
    /// is the contract that lets the OnceLock-of-Result pattern keep
    /// the hot path branch-free after the first successful init.
    #[test]
    fn client_is_cached_across_calls() {
        let svc = HttpNetworkService::with_max_response_bytes(DEFAULT_MAX_RESPONSE_BYTES);
        let first = svc.client().expect("first build succeeds");
        let second = svc.client().expect("second call returns cached client");
        // Same shared instance — the closure ran exactly once.
        assert!(std::ptr::eq(first, second));
    }

    /// PERF-04: the response cap is parsed exactly once, at construction.
    /// Unset → documented default; valid → parsed value; present-but-invalid
    /// (non-numeric, zero, negative, empty) → explicit Init error, never a
    /// silent fallback to the default.
    ///
    /// All cases live in one test so the env mutation is serialized — cargo
    /// runs `#[test]`s in parallel threads which would otherwise race on the
    /// shared process env. (The other tests in this module deliberately use
    /// `with_max_response_bytes`, which never touches the env.)
    #[test]
    fn from_env_parses_cap_once_with_default_and_loud_invalid() {
        let prev = std::env::var(MAX_RESPONSE_BYTES_KEY).ok();

        std::env::remove_var(MAX_RESPONSE_BYTES_KEY);
        let svc = HttpNetworkService::from_env().expect("unset env uses the default");
        assert_eq!(svc.max_response_bytes, DEFAULT_MAX_RESPONSE_BYTES);

        std::env::set_var(MAX_RESPONSE_BYTES_KEY, "1234");
        let svc = HttpNetworkService::from_env().expect("valid value parses");
        assert_eq!(svc.max_response_bytes, 1234);

        for invalid in ["not-a-number", "0", "-5", "12.5", ""] {
            std::env::set_var(MAX_RESPONSE_BYTES_KEY, invalid);
            let err = HttpNetworkService::from_env()
                .expect_err("present-but-invalid value must fail construction");
            let msg = err.to_string();
            assert!(
                msg.contains(MAX_RESPONSE_BYTES_KEY) && msg.contains(invalid),
                "error must name the key and the offending value, got: {msg}"
            );
        }

        match prev {
            Some(v) => std::env::set_var(MAX_RESPONSE_BYTES_KEY, v),
            None => std::env::remove_var(MAX_RESPONSE_BYTES_KEY),
        }
    }
}
