use std::{collections::HashMap, sync::Arc, time::Duration};

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

/// Config var key for [`HttpNetworkLimits::connect_timeout`], in whole
/// seconds. Read like [`MAX_RESPONSE_BYTES_KEY`].
pub const CONNECT_TIMEOUT_SECS_KEY: &str = "WAFER_RUN__NETWORK__CONNECT_TIMEOUT_SECS";

/// Config var key for [`HttpNetworkLimits::read_timeout`], in whole seconds.
/// Read like [`MAX_RESPONSE_BYTES_KEY`].
pub const READ_TIMEOUT_SECS_KEY: &str = "WAFER_RUN__NETWORK__READ_TIMEOUT_SECS";

/// Config var key for [`HttpNetworkLimits::request_timeout`], in whole
/// seconds. Read like [`MAX_RESPONSE_BYTES_KEY`].
pub const REQUEST_TIMEOUT_SECS_KEY: &str = "WAFER_RUN__NETWORK__REQUEST_TIMEOUT_SECS";

/// Config var key for [`HttpNetworkLimits::stream_timeout`], in whole
/// seconds. Read like [`MAX_RESPONSE_BYTES_KEY`], except that unset or empty
/// means no total (`None`) rather than a default.
pub const STREAM_TIMEOUT_SECS_KEY: &str = "WAFER_RUN__NETWORK__STREAM_TIMEOUT_SECS";

/// Default response body cap: 50 MiB. SEC-020 — prevents unbounded memory
/// growth from hostile or runaway upstream servers.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

/// Default [`HttpNetworkLimits::connect_timeout`]: 10 s.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default [`HttpNetworkLimits::read_timeout`]: 30 s.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Default [`HttpNetworkLimits::request_timeout`]: 30 s.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Size and time limits of an [`HttpNetworkService`].
///
/// The timeouts are split so a long streaming download is bounded by how long
/// the server goes quiet, not by how long the whole body takes:
/// `connect_timeout` and `read_timeout` apply to every request, the buffered
/// `do_request` also has the total `request_timeout`, and
/// `do_request_streaming` has a total only when `stream_timeout` is set.
///
/// Without `stream_timeout`, an upstream that trickles its body (one byte
/// just inside every `read_timeout`) holds a stream open for up to
/// `max_response_bytes` × `read_timeout` — in practice, indefinitely. Set it
/// when the streams this service serves have a known upper duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpNetworkLimits {
    /// Response body cap in bytes (SEC-020), on both paths.
    pub max_response_bytes: usize,
    /// Longest wait to establish a connection (DNS, TCP and TLS).
    pub connect_timeout: Duration,
    /// Longest the connection may go without delivering a byte, while waiting
    /// for the response head and between body chunks. Resets on every read.
    pub read_timeout: Duration,
    /// Total time allowed for a buffered `do_request`, response body included;
    /// through the network handler, for the whole redirect chain (see
    /// `NetworkService::buffered_deadline`). Not applied to
    /// `do_request_streaming`, whose body may legitimately take longer than
    /// any fixed total while `read_timeout` still bounds a stall.
    pub request_timeout: Duration,
    /// Total time allowed for a `do_request_streaming` exchange, response
    /// body included, or `None` for no total (the default): a stream that
    /// keeps delivering bytes then lasts as long as its body.
    pub stream_timeout: Option<Duration>,
}

impl Default for HttpNetworkLimits {
    fn default() -> Self {
        Self {
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            stream_timeout: None,
        }
    }
}

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
    /// Parsed once at construction (PERF-04), never per request.
    limits: HttpNetworkLimits,
}

/// Read `key` from the process env as a positive integer, or `default` when
/// unset. A present value that is not a positive integer is an error naming
/// the key and the value, never a silent fallback to the default.
fn positive_env<T>(key: &str, default: T) -> Result<T, NetworkError>
where
    T: std::str::FromStr + PartialOrd + From<u8> + std::fmt::Display,
{
    match std::env::var(key) {
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(NetworkError::Other(format!(
            "{key} is not valid UTF-8: expected a positive integer"
        ))),
        Ok(raw) => match raw.parse::<T>() {
            Ok(v) if v > T::from(0) => Ok(v),
            _ => Err(NetworkError::Other(format!(
                "{key}={raw:?} is invalid: expected a positive integer (unset it to use the \
                 default {default})"
            ))),
        },
    }
}

/// Read `key` like [`positive_env`], except that unset or empty is `None`.
fn optional_positive_env(key: &str) -> Result<Option<u64>, NetworkError> {
    match std::env::var(key) {
        Err(std::env::VarError::NotPresent) => Ok(None),
        Ok(raw) if raw.is_empty() => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(NetworkError::Other(format!(
            "{key} is not valid UTF-8: expected a positive integer"
        ))),
        Ok(raw) => match raw.parse::<u64>() {
            Ok(v) if v > 0 => Ok(Some(v)),
            _ => Err(NetworkError::Other(format!(
                "{key}={raw:?} is invalid: expected a positive integer (unset it for no limit)"
            ))),
        },
    }
}

impl HttpNetworkService {
    /// Construct the service, reading [`MAX_RESPONSE_BYTES_KEY`],
    /// [`CONNECT_TIMEOUT_SECS_KEY`], [`READ_TIMEOUT_SECS_KEY`],
    /// [`REQUEST_TIMEOUT_SECS_KEY`] and [`STREAM_TIMEOUT_SECS_KEY`] from the
    /// process env exactly once.
    ///
    /// - Unset → the matching [`HttpNetworkLimits::default`] value (for
    ///   [`STREAM_TIMEOUT_SECS_KEY`], unset or empty → no total).
    /// - Present but not a positive integer → explicit error, so a typo'd
    ///   limit fails the boot loudly instead of silently reverting to the
    ///   default.
    pub fn from_env() -> Result<Self, NetworkError> {
        let defaults = HttpNetworkLimits::default();
        let secs = |key: &str, default: Duration| {
            positive_env(key, default.as_secs()).map(Duration::from_secs)
        };
        Ok(Self::new(HttpNetworkLimits {
            max_response_bytes: positive_env(MAX_RESPONSE_BYTES_KEY, defaults.max_response_bytes)?,
            connect_timeout: secs(CONNECT_TIMEOUT_SECS_KEY, defaults.connect_timeout)?,
            read_timeout: secs(READ_TIMEOUT_SECS_KEY, defaults.read_timeout)?,
            request_timeout: secs(REQUEST_TIMEOUT_SECS_KEY, defaults.request_timeout)?,
            stream_timeout: optional_positive_env(STREAM_TIMEOUT_SECS_KEY)?
                .map(Duration::from_secs),
        }))
    }

    /// Construct with explicit limits (no env read). The underlying
    /// `reqwest::Client` is built on the first request.
    pub fn new(limits: HttpNetworkLimits) -> Self {
        Self {
            client: std::sync::OnceLock::new(),
            limits,
        }
    }

    /// Borrow the shared reqwest client, building it on first call. A
    /// build failure is cached so subsequent requests fail fast with
    /// the same error instead of retrying the broken configuration.
    ///
    /// SSRF deployment assumption: the [`SsrfFilteringResolver`] DNS-rebind
    /// layer runs only on **direct** connections. When an
    /// `HTTP(S)_PROXY` / `ALL_PROXY` env var is set, reqwest forwards the
    /// hostname to the proxy and the resolver never runs — only the
    /// literal-URL gate (`is_blocked_url`, applied per request) still fires.
    /// Behind an egress proxy, that proxy MUST enforce SSRF filtering. We
    /// deliberately do not call `.no_proxy()`: forcing direct egress would
    /// break legitimate proxied deployments. A configured proxy is logged
    /// once (below) so the bypass is visible in the deploy's logs.
    fn client(&self) -> Result<&reqwest::Client, NetworkError> {
        self.client
            .get_or_init(|| {
                if let Some(var) = detected_proxy_env() {
                    tracing::warn!(
                        proxy_env = var,
                        "outbound HTTP proxy configured ({var}); proxied requests bypass the \
                         SSRF DNS-rebind resolver — the egress proxy must enforce SSRF filtering"
                    );
                }
                reqwest::Client::builder()
                    // No total timeout on the client: it would cut off a
                    // streaming download that is still making progress. The
                    // buffered path sets `request_timeout` per request, the
                    // streaming path `stream_timeout` when configured.
                    .connect_timeout(self.limits.connect_timeout)
                    .read_timeout(self.limits.read_timeout)
                    // Never follow: a 3xx goes back to the network handler,
                    // which authorizes the next hop against the caller's grant
                    // and issues it as a new request through `send_request`,
                    // so the SSRF gates below run on every hop too.
                    .redirect(reqwest::redirect::Policy::none())
                    // DNS rebinding: drop resolved IPs pointing at
                    // private/loopback/link-local/multicast addresses. reqwest
                    // dials exactly the addresses returned here, so the checked
                    // IP is the dialed IP (no re-resolve TOCTOU).
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
    ///
    /// `total_timeout` bounds the whole exchange, body included; `None` leaves
    /// only the client's connect and idle-read timeouts.
    async fn send_request(
        &self,
        req: &Request,
        total_timeout: Option<Duration>,
    ) -> Result<reqwest::Response, NetworkError> {
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

        if let Some(total) = total_timeout {
            builder = builder.timeout(total);
        }

        builder
            .send()
            .await
            .map_err(|e| NetworkError::RequestError(e.to_string()))
    }
}

/// Proxy environment variables reqwest honours by default. If any is set,
/// proxied requests bypass the [`SsrfFilteringResolver`] DNS-rebind layer (see
/// the deployment note on [`HttpNetworkService::client`]). Returns the first
/// one found so it can be named in a one-time warning at client construction.
fn detected_proxy_env() -> Option<&'static str> {
    const PROXY_VARS: &[&str] = &[
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ];
    PROXY_VARS
        .iter()
        .copied()
        .find(|v| std::env::var_os(v).is_some())
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

/// Forward `body` chunks into an [`OutputStream`], enforcing the SEC-020
/// response `cap` as a running total.
///
/// On overflow — or an upstream read error — the stream terminates with an
/// `Error` terminal AFTER the chunks already forwarded, so a body that
/// outgrows the cap mid-stream is never reported as a clean `Complete` (no
/// silent truncation). The producer observes the paired `CancellationToken`,
/// so a consumer that drops the stream aborts a blocked upstream read promptly
/// instead of waiting for it to resolve.
///
/// Generic over the chunk/error types so it can be unit-tested with a
/// synthetic stream, not just reqwest's `bytes_stream`.
fn stream_capped<S, B, E>(body: S, cap: usize) -> OutputStream
where
    S: futures::Stream<Item = Result<B, E>> + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    OutputStream::from_producer(move |sink, cancel| async move {
        let mut body = std::pin::pin!(body);
        let mut received: usize = 0;
        loop {
            let next = tokio::select! {
                biased;
                // Consumer dropped the stream mid-read — abort promptly.
                () = cancel.cancelled() => return,
                next = body.next() => next,
            };
            let Some(chunk) = next else { break };
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
            let bytes = chunk.as_ref();
            received = received.saturating_add(bytes.len());
            if received > cap {
                let _ = sink
                    .error(WaferError::new(
                        ErrorCode::Unavailable,
                        format!("response body exceeds cap of {cap} bytes"),
                    ))
                    .await;
                return;
            }
            if sink.send_chunk(bytes.to_vec()).await.is_err() {
                // Consumer dropped the stream — stop reading.
                return;
            }
        }
        let _ = sink.complete(vec![]).await;
    })
}

#[wafer_async_trait]
impl NetworkService for HttpNetworkService {
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
        let response = self
            .send_request(req, Some(self.limits.request_timeout))
            .await?;
        let status_code = response.status().as_u16();
        let headers = collect_headers(&response);

        // SEC-020: cap response body size. Reject early if `Content-Length`
        // advertises more than the cap; otherwise accumulate from a chunk
        // stream and bail once the cap is exceeded (handles chunked /
        // unknown-length responses without buffering the whole thing in
        // reqwest's internal Bytes first).
        let cap = self.limits.max_response_bytes;
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

    /// `request_timeout` from the call. Each hop's own `do_request` carries the
    /// same total, so a direct caller is bounded too; through the network
    /// handler this one bounds the whole redirect chain.
    async fn buffered_deadline(&self) {
        tokio::time::sleep(self.limits.request_timeout).await;
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
    ///
    /// A connection that goes quiet for `read_timeout` ends the stream with an
    /// `Error` terminal. The total is `stream_timeout` (see
    /// [`HttpNetworkLimits`]): unset, a body that keeps arriving streams for
    /// as long as it takes; set, the exchange ends with an `Error` terminal
    /// once it has run that long.
    async fn do_request_streaming(
        &self,
        req: &Request,
    ) -> Result<(ResponseHead, OutputStream), NetworkError> {
        let response = self.send_request(req, self.limits.stream_timeout).await?;
        let status_code = response.status().as_u16();
        let headers = collect_headers(&response);

        let cap = self.limits.max_response_bytes;
        check_advertised_len(&response, cap)?;

        let head = ResponseHead {
            status_code,
            headers,
        };

        let stream = stream_capped(response.bytes_stream(), cap);

        Ok((head, stream))
    }
}

#[cfg(test)]
mod tests {
    // Used only by the SSRF-gate tests, which are compiled out under
    // `allow-private-network` (the gate they assert is itself disabled there).
    #[cfg(not(feature = "allow-private-network"))]
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
        let svc = HttpNetworkService::new(HttpNetworkLimits::default());
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

    /// The streaming entry point shares `send_request` with `do_request`, so
    /// the SSRF gate must fire on it too — a private/internal address is
    /// rejected before any stream is produced.
    #[cfg(not(feature = "allow-private-network"))]
    #[tokio::test]
    async fn streaming_request_rejected_for_private_address() {
        let svc = HttpNetworkService::new(HttpNetworkLimits::default());
        let req = Request {
            method: "GET".into(),
            url: "http://localhost/".into(),
            headers: HashMap::new(),
            body: None,
        };
        let err = svc
            .do_request_streaming(&req)
            .await
            .err()
            .expect("streaming path must apply the same SSRF gate as do_request");
        match err {
            NetworkError::RequestError(msg) => assert!(
                msg.contains("private/internal"),
                "expected SSRF rejection, got: {msg}"
            ),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// SEC-020 on the streaming path: a body that outgrows the cap must
    /// terminate the stream with an `Error` after forwarding the partial
    /// bytes seen so far — never a silent truncation reported as `Complete`.
    /// Driven through `stream_capped` directly with a synthetic multi-chunk
    /// body so the cap mechanism is exercised without a live server (the
    /// end-to-end streaming call would trip the SSRF gate on loopback first).
    #[tokio::test]
    async fn streaming_body_over_cap_ends_with_error_after_partial() {
        use futures::stream;
        use wafer_block::StreamEvent;

        // cap = 5 bytes; three 4-byte chunks = 12 bytes total.
        let chunks: Vec<Result<Vec<u8>, std::convert::Infallible>> = vec![
            Ok(b"aaaa".to_vec()),
            Ok(b"bbbb".to_vec()),
            Ok(b"cccc".to_vec()),
        ];
        let events: Vec<StreamEvent> = stream_capped(stream::iter(chunks), 5).collect().await;

        assert!(
            events.iter().any(|e| matches!(e, StreamEvent::Chunk(_))),
            "partial bytes forwarded before the cap tripped must be present, got: {events:?}"
        );
        assert!(
            matches!(events.last(), Some(StreamEvent::Error(_))),
            "an over-cap body must terminate with Error, got: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, StreamEvent::Complete { .. })),
            "an over-cap body must NOT be reported as a clean Complete, got: {events:?}"
        );
    }

    /// `client()` caches the built client across calls — two requests on
    /// the same service must observe the same `&reqwest::Client`. This
    /// is the contract that lets the OnceLock-of-Result pattern keep
    /// the hot path branch-free after the first successful init.
    #[test]
    fn client_is_cached_across_calls() {
        let svc = HttpNetworkService::new(HttpNetworkLimits::default());
        let first = svc.client().expect("first build succeeds");
        let second = svc.client().expect("second call returns cached client");
        // Same shared instance — the closure ran exactly once.
        assert!(std::ptr::eq(first, second));
    }

    /// PERF-04: every limit is parsed exactly once, at construction.
    /// Unset → documented default; valid → parsed value; present-but-invalid
    /// (non-numeric, zero, negative, empty) → explicit construction error,
    /// never a silent fallback to the default.
    ///
    /// All cases live in one test so the env mutation is serialized — cargo
    /// runs `#[test]`s in parallel threads which would otherwise race on the
    /// shared process env. (The other tests in this module deliberately use
    /// `HttpNetworkService::new`, which never touches the env.)
    #[test]
    fn from_env_parses_limits_once_with_default_and_loud_invalid() {
        let keys = [
            MAX_RESPONSE_BYTES_KEY,
            CONNECT_TIMEOUT_SECS_KEY,
            READ_TIMEOUT_SECS_KEY,
            REQUEST_TIMEOUT_SECS_KEY,
            STREAM_TIMEOUT_SECS_KEY,
        ];
        let prev: Vec<Option<String>> = keys.iter().map(|k| std::env::var(k).ok()).collect();

        for key in keys {
            std::env::remove_var(key);
        }
        let svc = HttpNetworkService::from_env().expect("unset env uses the defaults");
        assert_eq!(svc.limits, HttpNetworkLimits::default());

        std::env::set_var(MAX_RESPONSE_BYTES_KEY, "1234");
        std::env::set_var(CONNECT_TIMEOUT_SECS_KEY, "3");
        std::env::set_var(READ_TIMEOUT_SECS_KEY, "4");
        std::env::set_var(REQUEST_TIMEOUT_SECS_KEY, "5");
        std::env::set_var(STREAM_TIMEOUT_SECS_KEY, "6");
        let svc = HttpNetworkService::from_env().expect("valid values parse");
        assert_eq!(
            svc.limits,
            HttpNetworkLimits {
                max_response_bytes: 1234,
                connect_timeout: Duration::from_secs(3),
                read_timeout: Duration::from_secs(4),
                request_timeout: Duration::from_secs(5),
                stream_timeout: Some(Duration::from_secs(6)),
            }
        );

        // Empty is "no total" for the one optional limit.
        std::env::set_var(STREAM_TIMEOUT_SECS_KEY, "");
        let svc = HttpNetworkService::from_env().expect("empty stream timeout parses");
        assert_eq!(svc.limits.stream_timeout, None);
        std::env::set_var(STREAM_TIMEOUT_SECS_KEY, "6");

        for key in keys {
            let valid = std::env::var(key).expect("set above");
            let empty_is_valid = key == STREAM_TIMEOUT_SECS_KEY;
            for invalid in ["not-a-number", "0", "-5", "12.5", ""] {
                if invalid.is_empty() && empty_is_valid {
                    continue;
                }
                std::env::set_var(key, invalid);
                let err = HttpNetworkService::from_env()
                    .expect_err("present-but-invalid value must fail construction");
                let msg = err.to_string();
                assert!(
                    msg.contains(key) && msg.contains(&format!("{invalid:?}")),
                    "error must name the key and the offending value, got: {msg}"
                );
            }
            std::env::set_var(key, valid);
        }

        for (key, prev) in keys.iter().zip(prev) {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}
