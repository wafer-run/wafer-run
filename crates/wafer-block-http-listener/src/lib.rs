#![warn(missing_docs)]
//! HTTP transport block for WAFER: binds a TCP listener, converts incoming
//! HTTP requests into WAFER [`Message`]s + [`InputStream`]s, dispatches to a
//! configured flow or block, and converts the resulting [`OutputStream`] back
//! into an HTTP response.
//!
//! Registered as the `wafer-run/http-listener` block via
//! [`wafer_block::register_static_block!`]. The only public entry point most
//! consumers need is the block name itself; the [`http_to_message`] and
//! [`wafer_output_to_response`] helpers are re-exported for embedders that
//! bypass the listener (e.g. running a WAFER flow inside an existing axum
//! router).

use std::sync::OnceLock;

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, Method, StatusCode},
};
use parking_lot::Mutex;
use wafer_block::{
    http_codec, types::ConfigVar, Block, BlockInfo, InputStream, LifecycleEvent, LifecycleType,
    Message, OutputStream, WaferError,
};
use wafer_block_macro::wafer_async_trait;

// ---------------------------------------------------------------------------
// HTTP <-> Message conversion — thin axum glue over wafer_block::http_codec
// ---------------------------------------------------------------------------

/// Convert an HTTP request head into a WAFER [`Message`].
///
/// Axum adaptation of [`wafer_block::http_codec::build_http_message`] — the
/// canonical request-head → [`Message`] mapping (`http.*` meta, normalized
/// `req.*` meta, lowercased `http.header.*`, decoded `http.query.*` +
/// `req.query.*`) lives there. The body is **not** placed on the message —
/// it flows separately via [`InputStream`]. Headers whose values are not
/// valid UTF-8 are skipped.
pub fn http_to_message(
    method: &Method,
    uri_path: &str,
    raw_query: &str,
    headers: &HeaderMap,
    remote_addr: &str,
) -> Message {
    http_codec::build_http_message(
        method.as_str(),
        uri_path,
        raw_query,
        remote_addr,
        headers
            .iter()
            .filter_map(|(name, value)| value.to_str().ok().map(|v| (name.as_str(), v))),
    )
}

/// Collect a WAFER [`OutputStream`] and turn the terminal event into an
/// HTTP response.
///
/// Axum adaptation of [`wafer_block::http_codec::collect_http_response`],
/// which owns the canonical terminal-event mapping (`Complete`/`Halt` →
/// body+meta, `Error` → status from [`wafer_block::ErrorCode`] + JSON body,
/// `Drop` → `204`, `Continue` → empty `200`, `Malformed` → `500`). This
/// wrapper only rebuilds the transport-neutral parts as an
/// `axum::http::Response`.
pub async fn wafer_output_to_response(output: OutputStream) -> axum::http::Response<Body> {
    let parts = http_codec::collect_http_response(output).await;
    let mut builder = axum::http::Response::builder()
        .status(StatusCode::from_u16(parts.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
    for (name, value) in &parts.headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(parts.body))
        .unwrap_or_else(|_| internal_error_response())
}

fn internal_error_response() -> axum::http::Response<Body> {
    // `.body()` only errors on header-builder misuse; this hand-rolled
    // response sets neither headers nor an extension that could fail. The
    // expect documents the structural invariant rather than papering over
    // a runtime error case.
    axum::http::Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::from("internal server error"))
        .expect("static response body is always well-formed")
}

/// Build a plain-text error response with a fixed status. Used for transport
/// failures detected before dispatch (oversized / unreadable request bodies),
/// where there is no `OutputStream` to map.
fn status_text_response(status: StatusCode, message: &'static str) -> axum::http::Response<Body> {
    axum::http::Response::builder()
        .status(status)
        .body(Body::from(message))
        .unwrap_or_else(|_| internal_error_response())
}

/// True if `err` (or anything in its source chain) is a
/// [`http_body_util::LengthLimitError`].
///
/// `axum::body::to_bytes` wraps the body in `http_body_util::Limited`, so
/// exceeding the byte cap surfaces as an `axum::Error` whose source is a
/// `LengthLimitError`. Walking the chain lets us tell "body too large" apart
/// from a genuine transport read error (client disconnect, malformed
/// transfer-encoding).
fn is_length_limit_error(err: &axum::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = source {
        if e.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        source = e.source();
    }
    false
}

/// Map a request-body read failure to an HTTP response instead of silently
/// dispatching an empty body: exceeding `max_body_bytes` → `413 Payload Too
/// Large`; any other read failure → `400 Bad Request`. Either way the
/// discarded error is logged so the truncation is observable.
fn body_read_error_response(err: &axum::Error) -> axum::http::Response<Body> {
    if is_length_limit_error(err) {
        tracing::warn!(error = %err, "request body exceeds max_body_bytes; returning 413");
        status_text_response(StatusCode::PAYLOAD_TOO_LARGE, "request body too large")
    } else {
        tracing::warn!(error = %err, "failed to read request body; returning 400");
        status_text_response(StatusCode::BAD_REQUEST, "failed to read request body")
    }
}

// ---------------------------------------------------------------------------
// wafer-run/http-listener block
// ---------------------------------------------------------------------------

use wafer_block::config::DispatchTarget;

/// Default cap on request-body bytes buffered before dispatch — 10 MiB.
///
/// Single source of truth for the `max_body_bytes` default: rendered into the
/// `max_body_bytes` [`ConfigVar`] and used as the fallback when Init parses no
/// value.
const DEFAULT_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Block implementing the HTTP transport.
///
/// Singleton infrastructure block (one listener per registration). On
/// `LifecycleType::Init` it caches the `listen` socket address, the
/// `dispatch_target` (flow id or block name), and the `max_body_bytes` cap
/// from its [`BlockInfo`] config. The actual TCP bind + axum server is spawned
/// in [`Block::bind`] once the runtime hands over a `RuntimeHandle`, and is
/// shut down via a `tokio::sync::oneshot` channel on `LifecycleType::Stop`.
///
/// The `handle` method itself only returns `OutputStream::continue_with(msg)`;
/// real request handling happens inside the spawned axum task, not in the
/// block-message pipeline.
/// SEC-07: parse the `trusted_proxies` config (comma-separated IPs) into a list
/// of `IpAddr`, silently skipping blank/invalid entries.
fn parse_trusted_proxies(s: &str) -> Vec<std::net::IpAddr> {
    s.split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .filter_map(|e| e.parse::<std::net::IpAddr>().ok())
        .collect()
}

/// SEC-07: determine the client IP recorded on the message. Defaults to the
/// peer socket address; the rightmost `X-Forwarded-For` value is used only when
/// the direct peer is a configured trusted proxy — so a directly-connected
/// client cannot spoof its identity (used for IP rate limiting and audit) via
/// the header.
fn resolve_client_ip(
    peer: Option<std::net::IpAddr>,
    xff: Option<&str>,
    trusted_proxies: &[std::net::IpAddr],
) -> String {
    let peer_str = || peer.map_or_else(|| "unknown".to_string(), |ip| ip.to_string());
    if peer.is_some_and(|ip| trusted_proxies.contains(&ip)) {
        xff.and_then(|v| v.rsplit(',').next())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(peer_str)
    } else {
        peer_str()
    }
}

pub(crate) struct HttpListenerBlock {
    target: OnceLock<DispatchTarget>,
    listen: OnceLock<String>,
    max_body_bytes: OnceLock<usize>,
    /// SEC-07: IPs of trusted reverse proxies. `X-Forwarded-For` is honored
    /// only when the immediate peer is one of these; otherwise the peer socket
    /// address is used. Empty (default) = never trust the header.
    trusted_proxies: OnceLock<Vec<std::net::IpAddr>>,
    shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl Default for HttpListenerBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpListenerBlock {
    /// Construct an unconfigured listener. `target` and `listen` are filled
    /// in from [`BlockInfo`] config on the `Init` lifecycle event; the
    /// underlying TCP listener is not bound until [`Block::bind`] runs.
    pub(crate) fn new() -> Self {
        Self {
            target: OnceLock::new(),
            listen: OnceLock::new(),
            max_body_bytes: OnceLock::new(),
            trusted_proxies: OnceLock::new(),
            shutdown_tx: Mutex::new(None),
        }
    }
}

#[wafer_async_trait]
impl Block for HttpListenerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/http-listener",
            "0.0.1",
            "http-listener@v1",
            "HTTP transport — listens for HTTP requests and converts to messages",
        )
        .infrastructure()
        .flow_config(vec![
            ConfigVar::new(
                "listen",
                "Socket address the listener binds to (host:port).",
                "0.0.0.0:8080",
            )
            .name("Listen Address"),
            ConfigVar::new(
                "dispatch_target",
                "Default dispatch target (flow id or block name) when no \
                 explicit router upstream resolves the request.",
                "",
            )
            .name("Dispatch Target"),
            ConfigVar::new(
                "max_body_bytes",
                "Maximum request-body size in bytes buffered before dispatch. \
                 Larger bodies are truncated to empty.",
                &DEFAULT_MAX_BODY_BYTES.to_string(),
            )
            .name("Max Body Bytes"),
            ConfigVar::new(
                "trusted_proxies",
                "Comma-separated IP addresses of trusted reverse proxies. \
                 X-Forwarded-For is honored (for the client IP used in rate \
                 limiting and audit) only when the direct peer is one of these; \
                 otherwise the peer socket address is used. Empty = never trust \
                 the header (safe default for a directly-exposed listener).",
                "",
            )
            .name("Trusted Proxies"),
        ])
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::continue_with(msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init && self.target.get().is_none() {
            let config = wafer_block::BlockConfig::from_event(&event);

            if let Some(t) = config.dispatch_target() {
                self.target.set(t).ok();
            }
            self.listen.set(config.str("listen").to_string()).ok();
            let max_body = config
                .str("max_body_bytes")
                .parse::<usize>()
                .unwrap_or(DEFAULT_MAX_BODY_BYTES);
            self.max_body_bytes.set(max_body).ok();
            self.trusted_proxies
                .set(parse_trusted_proxies(config.str("trusted_proxies")))
                .ok();
        }

        if event.event_type == LifecycleType::Stop {
            if let Some(tx) = self.shutdown_tx.lock().take() {
                let _ = tx.send(());
            }
        }
        Ok(())
    }

    fn bind(&self, handle: Box<dyn std::any::Any + Send + Sync>) {
        let Ok(handle) = handle.downcast::<std::sync::Arc<dyn wafer_block::Runtime>>() else {
            return;
        };
        let handle = *handle;
        let Some(target) = self.target.get().cloned() else {
            return;
        };
        let listen = self.listen.get().cloned().unwrap_or_default();
        if listen.is_empty() {
            return;
        }
        let max_body_bytes = self
            .max_body_bytes
            .get()
            .copied()
            .unwrap_or(DEFAULT_MAX_BODY_BYTES);
        // SEC-07: shared across requests; `Arc` so the per-request closure
        // clones cheaply.
        let trusted_proxies =
            std::sync::Arc::new(self.trusted_proxies.get().cloned().unwrap_or_default());

        let (tx, rx) = tokio::sync::oneshot::channel();
        *self.shutdown_tx.lock() = Some(tx);

        tokio::spawn(async move {
            let handler = {
                let h = handle.clone();
                let target = target.clone();
                let trusted_proxies = trusted_proxies.clone();
                axum::routing::any(move |req: Request| {
                    let h = h.clone();
                    let target = target.clone();
                    let trusted_proxies = trusted_proxies.clone();
                    async move {
                        let (parts, body) = req.into_parts();
                        // Buffer the request body up to `max_body_bytes`. A read
                        // failure must NOT be collapsed into an empty body (the
                        // old `.unwrap_or_default()`): that silently masked
                        // "too large" and "connection dropped" as a legitimate
                        // empty request and let the handler return a misleading
                        // 2xx. Surface them as 413 / 400 instead.
                        let body_bytes = match axum::body::to_bytes(body, max_body_bytes).await {
                            Ok(b) => b.to_vec(),
                            Err(e) => return body_read_error_response(&e),
                        };

                        let uri = &parts.uri;
                        let path = uri.path();
                        let query = uri.query().unwrap_or("");
                        // SEC-07: the peer address comes from axum's
                        // `ConnectInfo` (wired via
                        // `into_make_service_with_connect_info` below). Default
                        // to it; trust `X-Forwarded-For` only from a configured
                        // trusted proxy so a direct client cannot spoof its IP.
                        let peer_ip = parts
                            .extensions
                            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                            .map(|ci| ci.0.ip());
                        let xff = parts
                            .headers
                            .get("x-forwarded-for")
                            .and_then(|v| v.to_str().ok());
                        let remote_addr = resolve_client_ip(peer_ip, xff, &trusted_proxies);

                        let msg = http_to_message(
                            &parts.method,
                            path,
                            query,
                            &parts.headers,
                            &remote_addr,
                        );
                        let input = InputStream::from_bytes(body_bytes);

                        let output = match &target {
                            DispatchTarget::Flow(fid) => h.run(fid, msg, input).await,
                            DispatchTarget::Block(name) => h.run_block(name, msg, input).await,
                        };
                        wafer_output_to_response(output).await
                    }
                })
            };

            let app = axum::Router::new()
                .route("/{*rest}", handler.clone())
                .route("/", handler);

            let listener = match tokio::net::TcpListener::bind(&listen).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("wafer-run/http-listener failed to bind {}: {}", listen, e);
                    return;
                }
            };

            // CONTRACT: See wafer-run/src/runtime/lifecycle.rs::start for the
            // full description. This event must remain
            // target = "wafer.runtime", event = "listening", with an `addr`
            // field carrying the bind address. Consumed by `wafer dev`'s boot
            // summary in wafer-cli/src/commands/dev/summary.rs.
            tracing::info!(
                target: "wafer.runtime",
                event = "listening",
                addr = %listen,
                "wafer-run/http-listener listening"
            );

            // SEC-07: `into_make_service_with_connect_info` injects the peer
            // `SocketAddr` into each request's extensions as
            // `ConnectInfo<SocketAddr>`, which the handler reads above. Without
            // it the peer address is never available and the client IP would
            // always fall back to whatever `X-Forwarded-For` claims.
            let serve = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = rx.await;
            });

            if let Err(e) = serve.await {
                tracing::error!("wafer-run/http-listener server error: {}", e);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

wafer_block::register_static_block!("wafer-run/http-listener", HttpListenerBlock);

// Query-decoding semantics (`+` → space, `%XX`, invalid-sequence tolerance)
// are pinned by table-driven tests next to the single implementation in
// `wafer_block::http_codec` (the former `url_decode_tests` moved there).

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn oversized_body_is_classified_and_mapped_to_413() {
        // A body larger than the limit makes `to_bytes` fail with a
        // LengthLimitError, which must map to 413 rather than an empty body.
        let body = Body::from(vec![0u8; 100]);
        let err = axum::body::to_bytes(body, 10)
            .await
            .expect_err("100 bytes over a 10-byte limit must error");
        assert!(
            is_length_limit_error(&err),
            "over-limit read should be a length-limit error"
        );
        let resp = body_read_error_response(&err);
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn under_limit_body_reads_successfully() {
        // Sanity: a body within the limit still reads as-is (no false 413).
        let body = Body::from(b"hello".to_vec());
        let bytes = axum::body::to_bytes(body, 1024)
            .await
            .expect("under-limit read should succeed");
        assert_eq!(&bytes[..], b"hello");
    }

    #[test]
    fn non_length_read_error_maps_to_400() {
        // A transport-style error (not a length limit) must surface as 400,
        // not be misreported as 413 or swallowed.
        let err = axum::Error::new(std::io::Error::other("connection reset"));
        assert!(!is_length_limit_error(&err));
        let resp = body_read_error_response(&err);
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // SEC-07
    use std::net::IpAddr;

    #[test]
    fn parse_trusted_proxies_skips_blank_and_invalid() {
        let v = parse_trusted_proxies(" 10.0.0.1, , not-an-ip, ::1 ");
        assert_eq!(
            v,
            vec![
                "10.0.0.1".parse::<IpAddr>().unwrap(),
                "::1".parse::<IpAddr>().unwrap()
            ]
        );
        assert!(parse_trusted_proxies("").is_empty());
    }

    #[test]
    fn client_ip_defaults_to_peer_and_ignores_xff_from_untrusted() {
        let peer: IpAddr = "203.0.113.5".parse().unwrap();
        // No trusted proxies configured: a direct client's X-Forwarded-For is
        // ignored; the peer address wins (no spoofing).
        assert_eq!(
            resolve_client_ip(Some(peer), Some("1.2.3.4"), &[]),
            "203.0.113.5"
        );
        // Peer not in the trusted set: XFF still ignored.
        let proxy: IpAddr = "10.0.0.1".parse().unwrap();
        assert_eq!(
            resolve_client_ip(Some(peer), Some("1.2.3.4"), &[proxy]),
            "203.0.113.5"
        );
    }

    #[test]
    fn client_ip_uses_xff_only_from_trusted_proxy() {
        let proxy: IpAddr = "10.0.0.1".parse().unwrap();
        // Peer IS the trusted proxy → the rightmost XFF entry (what the proxy
        // saw) is the real client.
        assert_eq!(
            resolve_client_ip(Some(proxy), Some("9.9.9.9, 8.8.8.8"), &[proxy]),
            "8.8.8.8"
        );
    }

    #[test]
    fn client_ip_unknown_without_peer() {
        assert_eq!(resolve_client_ip(None, Some("8.8.8.8"), &[]), "unknown");
    }
}
