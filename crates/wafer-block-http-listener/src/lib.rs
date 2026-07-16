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

use std::{net::IpAddr, sync::OnceLock};

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, Method, StatusCode},
};
use ipnet::IpNet;
use parking_lot::Mutex;
use wafer_block::{
    http_codec, types::ConfigVar, Block, BlockInfo, ErrorCode, InputStream, LifecycleEvent,
    LifecycleType, Message, OutputStream, WaferError,
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
/// `max_body_bytes` [`ConfigVar`] and used when Init finds no value. An
/// invalid value is a hard Init error, not a silent fall-back to this default.
const DEFAULT_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// SEC-07: parse the `trusted_proxies` config — comma-separated exact IPs
/// (`10.0.0.1`, `::1`) and/or CIDR ranges (`10.0.0.0/8`, `2001:db8::/32`) —
/// into a list of [`IpNet`]s. Exact IPs become full-length prefixes
/// (`/32` for IPv4, `/128` for IPv6).
///
/// Blank entries (leading/trailing/double commas) are skipped; any other
/// unparseable entry is a **configuration error** naming the entry. Absent
/// config means "trust no proxies", but a present-and-invalid entry must fail
/// loud at load — silently dropping it would run the listener with fewer
/// trusted proxies than the operator configured, breaking client-IP
/// attribution for rate limiting and audit without any signal.
fn parse_trusted_proxies(s: &str) -> Result<Vec<IpNet>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| {
            e.parse::<IpAddr>()
                .map(IpNet::from)
                .or_else(|_| e.parse::<IpNet>())
                .map_err(|_| {
                    format!(
                        "invalid trusted_proxies entry '{e}': expected an IP address \
                         (e.g. 10.0.0.1, ::1) or CIDR range (e.g. 10.0.0.0/8, 2001:db8::/32)"
                    )
                })
        })
        .collect()
}

/// SEC-07: whether `ip` falls inside any configured trusted-proxy entry
/// (exact IPs are full-length prefixes, so one containment check covers both).
fn is_trusted_proxy(ip: IpAddr, trusted_proxies: &[IpNet]) -> bool {
    trusted_proxies.iter().any(|net| net.contains(&ip))
}

/// SEC-07: determine the client IP recorded on the message.
///
/// Defaults to the peer socket address. Only when the direct peer is a
/// configured trusted proxy is `X-Forwarded-For` consulted, using the
/// rightmost-untrusted algorithm: walk the chain right to left, skipping
/// entries that are themselves trusted proxies (each appended its upstream);
/// the first non-trusted entry is the client. If every entry is a trusted
/// proxy, the leftmost wins. A malformed entry terminates the walk with a
/// fall-back to the peer address — everything to the left of garbage is
/// attacker-suppliable (any hop controls what appears left of itself), so
/// none of it may be trusted.
///
/// Net effect: a directly-connected client can never spoof its identity
/// (used for IP rate limiting and audit) via the header, and a client behind
/// trusted proxies cannot smuggle a fake hop past them.
fn resolve_client_ip(peer: Option<IpAddr>, xff: Option<&str>, trusted_proxies: &[IpNet]) -> String {
    let peer_str = || peer.map_or_else(|| "unknown".to_string(), |ip| ip.to_string());
    // Fail-safe: peer unknown or not a trusted proxy → the peer identity
    // stands and X-Forwarded-For is ignored entirely.
    if !peer.is_some_and(|ip| is_trusted_proxy(ip, trusted_proxies)) {
        return peer_str();
    }
    let Some(xff) = xff else {
        return peer_str();
    };
    // Rightmost-untrusted walk. `leftmost_trusted` tracks the most recently
    // seen (i.e. furthest-left) trusted hop so an all-trusted chain resolves
    // to its leftmost entry; an empty header leaves it `None` → peer.
    let mut leftmost_trusted: Option<IpAddr> = None;
    for entry in xff.rsplit(',').map(str::trim) {
        match entry.parse::<IpAddr>() {
            Ok(ip) if is_trusted_proxy(ip, trusted_proxies) => leftmost_trusted = Some(ip),
            Ok(ip) => return ip.to_string(),
            // Malformed hop (including empty segments): stop peeling, trust
            // nothing further left, attribute to the peer.
            Err(_) => return peer_str(),
        }
    }
    leftmost_trusted.map_or_else(peer_str, |ip| ip.to_string())
}

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
pub(crate) struct HttpListenerBlock {
    target: OnceLock<DispatchTarget>,
    listen: OnceLock<String>,
    max_body_bytes: OnceLock<usize>,
    /// SEC-07: trusted reverse proxies as exact IPs and/or CIDR ranges.
    /// `X-Forwarded-For` is honored only when the immediate peer matches one
    /// of these; otherwise the peer socket address is used. Empty (default) =
    /// never trust the header.
    trusted_proxies: OnceLock<Vec<IpNet>>,
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
                 Larger bodies are rejected with 413 Payload Too Large.",
                &DEFAULT_MAX_BODY_BYTES.to_string(),
            )
            .name("Max Body Bytes"),
            ConfigVar::new(
                "trusted_proxies",
                "Comma-separated trusted reverse proxies: exact IPs (10.0.0.1, \
                 ::1) and/or CIDR ranges (10.0.0.0/8, 2001:db8::/32). \
                 X-Forwarded-For is honored (for the client IP used in rate \
                 limiting and audit) only when the direct peer matches one of \
                 these; the chain is then peeled right-to-left across trusted \
                 hops. Otherwise the peer socket address is used. Empty = never \
                 trust the header (safe default for a directly-exposed \
                 listener). Invalid entries fail Init.",
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

            // SEC-07: validate `trusted_proxies` BEFORE caching any other
            // state. On error nothing is set — in particular `target` — so
            // `bind()` refuses to start the server. An invalid security
            // config must fail loud (absent = default, present-but-invalid =
            // error), never silently degrade to "trust nothing".
            let trusted = parse_trusted_proxies(config.str("trusted_proxies"))
                .map_err(|msg| WaferError::new(ErrorCode::InvalidArgument, msg))?;
            self.trusted_proxies.set(trusted).ok();

            if let Some(t) = config.dispatch_target() {
                self.target.set(t).ok();
            }
            self.listen.set(config.str("listen").to_string()).ok();
            // Config rule (same as `trusted_proxies` above): absent = the
            // documented default; present-but-invalid = a loud Init error, not
            // a silent fall-back to the default.
            let max_body = match config.str("max_body_bytes") {
                "" => DEFAULT_MAX_BODY_BYTES,
                raw => raw.parse::<usize>().map_err(|_| {
                    WaferError::new(
                        ErrorCode::InvalidArgument,
                        format!(
                            "max_body_bytes={raw:?} is not a valid byte count \
                             (unset it for the default {DEFAULT_MAX_BODY_BYTES})"
                        ),
                    )
                })?,
            };
            self.max_body_bytes.set(max_body).ok();
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

    // ── SEC-07: trusted_proxies parsing (exact IPs + CIDR) ────────────────

    /// Test helper: parse a trusted-proxies config string that must be valid.
    fn proxies(s: &str) -> Vec<IpNet> {
        parse_trusted_proxies(s).expect("test trusted_proxies config must parse")
    }

    #[test]
    fn parse_accepts_exact_ips_and_skips_blanks() {
        let v = proxies(" 10.0.0.1, , ::1 , ");
        assert_eq!(v.len(), 2);
        // Exact IPs become full-length prefixes.
        assert_eq!(v[0], "10.0.0.1/32".parse::<IpNet>().unwrap());
        assert_eq!(v[1], "::1/128".parse::<IpNet>().unwrap());
        assert!(proxies("").is_empty());
        assert!(proxies("   ").is_empty());
    }

    #[test]
    fn parse_accepts_v4_and_v6_cidr_ranges() {
        let v = proxies("10.0.0.0/8, 2001:db8::/32, 192.168.1.1");
        assert_eq!(v[0], "10.0.0.0/8".parse::<IpNet>().unwrap());
        assert_eq!(v[1], "2001:db8::/32".parse::<IpNet>().unwrap());
        assert_eq!(v[2], "192.168.1.1/32".parse::<IpNet>().unwrap());
    }

    #[test]
    fn parse_rejects_invalid_entries_naming_the_entry() {
        // Present-but-invalid must fail loud (config rule), not be skipped.
        for bad in [
            "not-an-ip",
            "10.0.0.0/33",    // v4 prefix out of range
            "2001:db8::/129", // v6 prefix out of range
            "10.0.0.256",     // invalid octet
            "10.0.0.1:8080",  // socket address, not an IP
            "example.com",    // hostname, not an IP
            "10.0.0.0/8/8",   // double prefix
        ] {
            let err = parse_trusted_proxies(&format!("10.0.0.1, {bad}, ::1"))
                .expect_err("invalid entry must be a config error");
            assert!(
                err.contains(bad),
                "error must name the bad entry {bad:?}, got: {err}"
            );
        }
    }

    #[test]
    fn cidr_matching_covers_boundaries_v4_and_v6() {
        let v = proxies("10.0.0.0/8, 2001:db8::/32");
        let contained = |s: &str| is_trusted_proxy(s.parse::<IpAddr>().unwrap(), &v);
        // v4: first and last address of 10.0.0.0/8 are in; neighbors are out.
        assert!(contained("10.0.0.0"));
        assert!(contained("10.255.255.255"));
        assert!(!contained("9.255.255.255"));
        assert!(!contained("11.0.0.0"));
        // v6: first and last address of 2001:db8::/32 are in; neighbors out.
        assert!(contained("2001:db8::"));
        assert!(contained("2001:db8:ffff:ffff:ffff:ffff:ffff:ffff"));
        assert!(!contained("2001:db7:ffff:ffff:ffff:ffff:ffff:ffff"));
        assert!(!contained("2001:db9::"));
        // A v4 client never matches a v6 range and vice versa.
        assert!(!is_trusted_proxy(
            "10.0.0.1".parse().unwrap(),
            &proxies("2001:db8::/32")
        ));
        assert!(!is_trusted_proxy(
            "2001:db8::1".parse().unwrap(),
            &proxies("10.0.0.0/8")
        ));
    }

    #[test]
    fn exact_ip_entries_match_only_themselves() {
        let v = proxies("10.0.0.1");
        assert!(is_trusted_proxy("10.0.0.1".parse().unwrap(), &v));
        assert!(!is_trusted_proxy("10.0.0.2".parse().unwrap(), &v));
    }

    // ── SEC-07: client-IP resolution (rightmost-untrusted XFF peeling) ────

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
        assert_eq!(
            resolve_client_ip(Some(peer), Some("1.2.3.4"), &proxies("10.0.0.1")),
            "203.0.113.5"
        );
        // Peer just outside a trusted CIDR: XFF still ignored.
        assert_eq!(
            resolve_client_ip(Some(peer), Some("1.2.3.4"), &proxies("203.0.113.6/31")),
            "203.0.113.5"
        );
    }

    #[test]
    fn client_ip_single_hop_from_trusted_proxy() {
        let proxy: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = proxies("10.0.0.1");
        // Peer IS the trusted proxy → the single XFF entry is the client.
        assert_eq!(
            resolve_client_ip(Some(proxy), Some("9.9.9.9"), &trusted),
            "9.9.9.9"
        );
        // Rightmost entry is not a trusted proxy → it is the client, even
        // with more (attacker-suppliable) entries to its left.
        assert_eq!(
            resolve_client_ip(Some(proxy), Some("1.2.3.4, 8.8.8.8"), &trusted),
            "8.8.8.8"
        );
    }

    #[test]
    fn client_ip_peels_multi_hop_chain_across_trusted_intermediates() {
        // Chain: client 9.9.9.9 → proxy 10.0.0.3 → proxy 10.0.0.2 → peer
        // 10.0.0.1. Each hop appended its upstream, so XFF is
        // "9.9.9.9, 10.0.0.3, 10.0.0.2". Peeling right-to-left skips the
        // trusted intermediates and lands on the client.
        let trusted = proxies("10.0.0.0/24");
        assert_eq!(
            resolve_client_ip(
                Some("10.0.0.1".parse().unwrap()),
                Some("9.9.9.9, 10.0.0.3, 10.0.0.2"),
                &trusted
            ),
            "9.9.9.9"
        );
        // The spoof attempt "1.2.3.4" left of the real client is ignored:
        // peeling stops at the first (rightmost) untrusted entry.
        assert_eq!(
            resolve_client_ip(
                Some("10.0.0.1".parse().unwrap()),
                Some("1.2.3.4, 9.9.9.9, 10.0.0.2"),
                &trusted
            ),
            "9.9.9.9"
        );
        // IPv6 client through IPv6 trusted proxies.
        assert_eq!(
            resolve_client_ip(
                Some("2001:db8::1".parse().unwrap()),
                Some("2001:4860::8888, 2001:db8::2"),
                &proxies("2001:db8::/32")
            ),
            "2001:4860::8888"
        );
    }

    #[test]
    fn client_ip_all_trusted_chain_uses_leftmost() {
        // Every XFF entry is a trusted proxy (e.g. health checks between
        // proxies): the leftmost entry is the best client identity available.
        let trusted = proxies("10.0.0.0/24");
        assert_eq!(
            resolve_client_ip(
                Some("10.0.0.1".parse().unwrap()),
                Some("10.0.0.4, 10.0.0.3, 10.0.0.2"),
                &trusted
            ),
            "10.0.0.4"
        );
    }

    #[test]
    fn client_ip_malformed_entry_terminates_walk_at_peer() {
        let trusted = proxies("10.0.0.0/24");
        let peer: Option<IpAddr> = Some("10.0.0.1".parse().unwrap());
        // Garbage mid-chain: the rightmost hop is trusted (skipped), then the
        // malformed hop stops the walk — everything left of garbage
        // (including the plausible-looking 9.9.9.9) is untrustworthy.
        assert_eq!(
            resolve_client_ip(peer, Some("9.9.9.9, garbage, 10.0.0.2"), &trusted),
            "10.0.0.1"
        );
        // Rightmost entry malformed: nothing peels; fall back to peer.
        assert_eq!(
            resolve_client_ip(peer, Some("9.9.9.9, not-an-ip"), &trusted),
            "10.0.0.1"
        );
        // Port-suffixed and empty segments are malformed, not lenient-parsed.
        assert_eq!(
            resolve_client_ip(peer, Some("9.9.9.9:1234"), &trusted),
            "10.0.0.1"
        );
        assert_eq!(
            resolve_client_ip(peer, Some("9.9.9.9,, 10.0.0.2"), &trusted),
            "10.0.0.1"
        );
    }

    #[test]
    fn client_ip_empty_or_missing_xff_falls_back_to_peer() {
        let trusted = proxies("10.0.0.1");
        let peer: Option<IpAddr> = Some("10.0.0.1".parse().unwrap());
        assert_eq!(resolve_client_ip(peer, None, &trusted), "10.0.0.1");
        assert_eq!(resolve_client_ip(peer, Some(""), &trusted), "10.0.0.1");
        assert_eq!(resolve_client_ip(peer, Some("   "), &trusted), "10.0.0.1");
    }

    #[test]
    fn client_ip_unknown_without_peer() {
        // No peer address at all: XFF is never consulted, even if proxies
        // are configured.
        assert_eq!(resolve_client_ip(None, Some("8.8.8.8"), &[]), "unknown");
        assert_eq!(
            resolve_client_ip(None, Some("8.8.8.8"), &proxies("10.0.0.1")),
            "unknown"
        );
    }

    // ── SEC-07: Init fails loud on invalid trusted_proxies config ─────────

    /// Minimal `Context` impl for driving `lifecycle()` directly; the
    /// listener's Init path never touches the context.
    struct NoopCtx;

    #[wafer_async_trait]
    impl wafer_block::context::Context for NoopCtx {
        async fn call_block(
            &self,
            _block_name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            unimplemented!("listener Init does not call blocks")
        }

        fn is_cancelled(&self) -> bool {
            false
        }

        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }

        fn clone_arc(&self) -> std::sync::Arc<dyn wafer_block::context::Context> {
            unimplemented!("listener Init does not clone the context")
        }
    }

    fn init_event(config: &serde_json::Value) -> LifecycleEvent {
        LifecycleEvent {
            event_type: LifecycleType::Init,
            data: serde_json::to_vec(config).expect("test config serializes"),
        }
    }

    #[tokio::test]
    async fn init_rejects_invalid_trusted_proxies_and_caches_nothing() {
        let block = HttpListenerBlock::new();
        let event = init_event(&serde_json::json!({
            "listen": "127.0.0.1:0",
            "flow": "some-flow",
            "trusted_proxies": "10.0.0.1, bogus/99",
        }));
        let err = block
            .lifecycle(&NoopCtx, event)
            .await
            .expect_err("invalid trusted_proxies must fail Init");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(
            err.message.contains("bogus/99"),
            "error must name the bad entry, got: {}",
            err.message
        );
        // Nothing was cached — `bind()` would refuse to start the server.
        assert!(block.target.get().is_none());
        assert!(block.listen.get().is_none());
        assert!(block.trusted_proxies.get().is_none());
    }

    #[tokio::test]
    async fn init_accepts_valid_trusted_proxies() {
        let block = HttpListenerBlock::new();
        let event = init_event(&serde_json::json!({
            "listen": "127.0.0.1:0",
            "flow": "some-flow",
            "trusted_proxies": "10.0.0.0/8, ::1",
        }));
        block
            .lifecycle(&NoopCtx, event)
            .await
            .expect("valid trusted_proxies must pass Init");
        assert_eq!(
            block.trusted_proxies.get().expect("set at Init"),
            &proxies("10.0.0.0/8, ::1")
        );
    }

    /// Config rule: a present-but-invalid `max_body_bytes` fails Init loudly,
    /// naming the bad value, rather than silently falling back to the default.
    #[tokio::test]
    async fn init_rejects_invalid_max_body_bytes() {
        let block = HttpListenerBlock::new();
        let event = init_event(&serde_json::json!({
            "listen": "127.0.0.1:0",
            "flow": "some-flow",
            "max_body_bytes": "ten-megs",
        }));
        let err = block
            .lifecycle(&NoopCtx, event)
            .await
            .expect_err("invalid max_body_bytes must fail Init");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(
            err.message.contains("ten-megs"),
            "error must name the bad value, got: {}",
            err.message
        );
        assert!(block.max_body_bytes.get().is_none());
    }

    /// Absent `max_body_bytes` uses the documented default (no error).
    #[tokio::test]
    async fn init_absent_max_body_bytes_uses_default() {
        let block = HttpListenerBlock::new();
        let event = init_event(&serde_json::json!({
            "listen": "127.0.0.1:0",
            "flow": "some-flow",
        }));
        block
            .lifecycle(&NoopCtx, event)
            .await
            .expect("absent max_body_bytes must pass Init");
        assert_eq!(
            block.max_body_bytes.get().copied(),
            Some(DEFAULT_MAX_BODY_BYTES)
        );
    }
}
