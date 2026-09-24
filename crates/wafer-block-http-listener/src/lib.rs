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

use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{Arc, OnceLock},
    task::Poll,
    time::Duration,
};

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, Method, StatusCode},
};
use hyper::{body::Incoming, server::conn::http1};
use hyper_util::rt::{TokioIo, TokioTimer};
use ipnet::IpNet;
use parking_lot::Mutex;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch, OwnedSemaphorePermit, Semaphore},
    task::{JoinHandle, JoinSet},
};
use tower::ServiceExt;
use wafer_block::{
    http_codec, types::ConfigVar, Block, BlockConfig, BlockInfo, ErrorCode, InputStream,
    LifecycleEvent, LifecycleType, Message, OutputStream, WaferError,
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
const DEFAULT_MAX_BODY_BYTES: u64 = 10 * 1024 * 1024;

/// Default time a client has to deliver a complete request head (request
/// line and headers) — 30 s. hyper starts this timer at every head read,
/// so it also bounds how long an idle keep-alive connection is held open
/// waiting for its next request.
const DEFAULT_HEADER_READ_TIMEOUT_SECS: u64 = 30;

/// Default time a client has to deliver the whole request body once the
/// head has arrived — 120 s (a 10 MiB body needs ~87 KB/s).
const DEFAULT_BODY_READ_TIMEOUT_SECS: u64 = 120;

/// Default cap on concurrently open client connections.
const DEFAULT_MAX_CONNECTIONS: u64 = 1024;

/// Default time a response write may make no progress — 60 s. A client that
/// stops reading its response is dropped after this, freeing its slot.
const DEFAULT_WRITE_TIMEOUT_SECS: u64 = 60;

/// Default time a stopping listener gives open connections to finish their
/// in-flight request — 10 s. Connections still open after it are aborted.
const DEFAULT_SHUTDOWN_GRACE_SECS: u64 = 10;

/// Upper bound for every timeout knob — one day. Larger values are refused
/// at Init: they bound nothing a real client needs, and a deadline of
/// `now + u64::MAX` seconds overflows `Instant`.
const MAX_TIMEOUT_SECS: u64 = 24 * 60 * 60;

/// Read a positive integer knob from Init config.
///
/// Absent, `null` or `""` = `default`. A JSON number or a decimal string in
/// `1..=max` is accepted (block config arrives both as typed JSON from
/// `add_block_config` and as strings from flow `config_map`). Anything else
/// — `0` included, since every knob read here is a bound and `0` would
/// either disable the listener or be misread as "unlimited" — is an
/// `InvalidArgument` naming the key and the value.
fn bounded_config_int(
    config: &BlockConfig,
    key: &str,
    default: u64,
    max: u64,
) -> Result<u64, WaferError> {
    let parsed = match config.get(key) {
        None | Some(serde_json::Value::Null) => return Ok(default),
        Some(serde_json::Value::String(s)) if s.is_empty() => return Ok(default),
        Some(serde_json::Value::String(s)) => s.trim().parse::<u64>().ok(),
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        Some(_) => None,
    };
    match parsed {
        Some(n) if (1..=max).contains(&n) => Ok(n),
        _ => Err(WaferError::new(
            ErrorCode::InvalidArgument,
            format!(
                "{key}={} is not an integer in 1..={max} (unset it for the default {default})",
                config
                    .get(key)
                    .map_or_else(String::new, ToString::to_string),
            ),
        )),
    }
}

/// SEC-07: parse the `trusted_proxies` config — comma-separated exact IPs
/// (`10.0.0.1`, `::1`) and/or CIDR ranges (`10.0.0.0/8`, `2001:db8::/32`) —
/// into a list of [`IpNet`]s. Exact IPs become full-length prefixes
/// (`/32` for IPv4, `/128` for IPv6); an IPv4-mapped IPv6 address
/// (`::ffff:10.0.0.1`) is stored as the IPv4 address it maps.
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
                .map(|ip| IpNet::from(ip.to_canonical()))
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
/// `xff_lines` is every `X-Forwarded-For` field line in wire order; `None`
/// stands for a line that is not visible ASCII. Repeated field lines are one
/// comma-separated list (RFC 9110 §5.3), and a proxy may append its hop as a
/// line of its own (HAProxy `option forwardfor`) rather than extending the
/// last one, so the walk covers every line — reading only the first would
/// take the client-written line as the client IP.
///
/// Defaults to the peer socket address. Only when the direct peer is a
/// configured trusted proxy is the chain consulted, using the
/// rightmost-untrusted algorithm: walk the chain right to left, skipping
/// entries that are themselves trusted proxies (each appended its upstream);
/// the first non-trusted entry is the client. If every entry is a trusted
/// proxy, the leftmost wins. A malformed entry (or a non-text line)
/// terminates the walk with a fall-back to the peer address — everything to
/// the left of garbage is attacker-suppliable (any hop controls what appears
/// left of itself), so none of it may be trusted.
///
/// The peer and every entry are compared in canonical form
/// ([`IpAddr::to_canonical`]): a listener bound to `[::]` sees IPv4 peers as
/// IPv4-mapped IPv6 addresses (`::ffff:10.0.0.1`), which must still match a
/// trusted `10.0.0.1`, and the recorded client IP is the plain IPv4 address.
///
/// Net effect: a directly-connected client can never spoof its identity
/// (used for IP rate limiting and audit) via the header, and a client behind
/// trusted proxies cannot smuggle a fake hop past them.
fn resolve_client_ip<'a>(
    peer: Option<IpAddr>,
    xff_lines: impl DoubleEndedIterator<Item = Option<&'a str>>,
    trusted_proxies: &[IpNet],
) -> String {
    let peer = peer.map(|ip| ip.to_canonical());
    let peer_str = || peer.map_or_else(|| "unknown".to_string(), |ip| ip.to_string());
    // Fail-safe: peer unknown or not a trusted proxy → the peer identity
    // stands and X-Forwarded-For is ignored entirely.
    if !peer.is_some_and(|ip| is_trusted_proxy(ip, trusted_proxies)) {
        return peer_str();
    }
    // Rightmost-untrusted walk. `leftmost_trusted` tracks the most recently
    // seen (i.e. furthest-left) trusted hop so an all-trusted chain resolves
    // to its leftmost entry; no header at all leaves it `None` → peer.
    let mut leftmost_trusted: Option<IpAddr> = None;
    for line in xff_lines.rev() {
        let Some(line) = line else {
            // A line that is not text is a malformed hop.
            return peer_str();
        };
        for entry in line.rsplit(',').map(str::trim) {
            match entry.parse::<IpAddr>().map(|ip| ip.to_canonical()) {
                Ok(ip) if is_trusted_proxy(ip, trusted_proxies) => leftmost_trusted = Some(ip),
                Ok(ip) => return ip.to_string(),
                // Malformed hop (including empty segments and empty lines):
                // stop peeling, trust nothing further left, attribute to the
                // peer.
                Err(_) => return peer_str(),
            }
        }
    }
    leftmost_trusted.map_or_else(peer_str, |ip| ip.to_string())
}

/// Per-connection and per-request bounds, resolved once at Init.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequestLimits {
    /// Request-body bytes buffered before dispatch; more → 413.
    max_body_bytes: usize,
    /// Time allowed for a request head (and for an idle keep-alive wait).
    header_read_timeout: Duration,
    /// Time allowed for the whole body once the head arrived; more → 408.
    body_read_timeout: Duration,
    /// Concurrently open client connections; at the cap the listener stops
    /// accepting and further clients queue in the kernel's accept backlog.
    max_connections: usize,
    /// Time a response write may make no progress before the connection is
    /// dropped.
    write_timeout: Duration,
    /// Time a stopping listener waits for open connections to finish.
    shutdown_grace: Duration,
}

/// Everything Init resolves from the listener's config. Built only when every
/// value is valid, then stored once — a failed Init leaves nothing behind, so
/// [`Block::bind`] refuses to start a half-configured server.
#[derive(Debug)]
struct ListenerSettings {
    target: Option<DispatchTarget>,
    listen: String,
    /// SEC-07: trusted reverse proxies as exact IPs and/or CIDR ranges.
    /// `X-Forwarded-For` is honored only when the immediate peer matches one
    /// of these; otherwise the peer socket address is used. Empty (default) =
    /// never trust the header.
    trusted_proxies: Vec<IpNet>,
    limits: RequestLimits,
}

impl ListenerSettings {
    /// Config rule for every key: absent = the documented default;
    /// present-but-invalid = a loud Init error naming the value, never a
    /// silent fall-back.
    fn from_config(config: &BlockConfig) -> Result<Self, WaferError> {
        let trusted_proxies = parse_trusted_proxies(config.str("trusted_proxies"))
            .map_err(|msg| WaferError::new(ErrorCode::InvalidArgument, msg))?;
        let max_body_bytes =
            bounded_config_int(config, "max_body_bytes", DEFAULT_MAX_BODY_BYTES, u64::MAX)?;
        let header_read_timeout = bounded_config_int(
            config,
            "header_read_timeout_secs",
            DEFAULT_HEADER_READ_TIMEOUT_SECS,
            MAX_TIMEOUT_SECS,
        )?;
        let body_read_timeout = bounded_config_int(
            config,
            "body_read_timeout_secs",
            DEFAULT_BODY_READ_TIMEOUT_SECS,
            MAX_TIMEOUT_SECS,
        )?;
        // `Semaphore::new` panics above `MAX_PERMITS`.
        let max_connections = bounded_config_int(
            config,
            "max_connections",
            DEFAULT_MAX_CONNECTIONS,
            Semaphore::MAX_PERMITS as u64,
        )?;
        let write_timeout = bounded_config_int(
            config,
            "write_timeout_secs",
            DEFAULT_WRITE_TIMEOUT_SECS,
            MAX_TIMEOUT_SECS,
        )?;
        let shutdown_grace = bounded_config_int(
            config,
            "shutdown_grace_secs",
            DEFAULT_SHUTDOWN_GRACE_SECS,
            MAX_TIMEOUT_SECS,
        )?;
        let usize_of = |key: &str, n: u64| {
            usize::try_from(n).map_err(|_| {
                WaferError::new(
                    ErrorCode::InvalidArgument,
                    format!("{key}={n} does not fit this platform's address space"),
                )
            })
        };
        Ok(Self {
            target: config.dispatch_target(),
            listen: config.str("listen").to_string(),
            trusted_proxies,
            limits: RequestLimits {
                max_body_bytes: usize_of("max_body_bytes", max_body_bytes)?,
                header_read_timeout: Duration::from_secs(header_read_timeout),
                body_read_timeout: Duration::from_secs(body_read_timeout),
                max_connections: usize_of("max_connections", max_connections)?,
                write_timeout: Duration::from_secs(write_timeout),
                shutdown_grace: Duration::from_secs(shutdown_grace),
            },
        })
    }
}

/// What every request handler shares: where to dispatch and how to read the
/// request.
struct RequestContext {
    runtime: Arc<dyn wafer_block::Runtime>,
    target: DispatchTarget,
    trusted_proxies: Vec<IpNet>,
    limits: RequestLimits,
}

/// `408 Request Timeout` for a body that did not arrive within
/// `body_read_timeout_secs`. `Connection: close` because the rest of the
/// body may still be in flight: the connection cannot carry another request.
fn body_timeout_response(timeout: Duration) -> axum::http::Response<Body> {
    tracing::warn!(
        timeout_secs = timeout.as_secs(),
        "request body not received within body_read_timeout_secs; returning 408"
    );
    axum::http::Response::builder()
        .status(StatusCode::REQUEST_TIMEOUT)
        .header(axum::http::header::CONNECTION, "close")
        .body(Body::from("request body timed out"))
        .unwrap_or_else(|_| internal_error_response())
}

/// Turn one HTTP request into a WAFER dispatch and its output into the
/// response.
async fn dispatch_request(cx: Arc<RequestContext>, req: Request) -> axum::http::Response<Body> {
    let (parts, body) = req.into_parts();
    // Buffer the request body up to `max_body_bytes` within
    // `body_read_timeout`. A read failure must NOT be collapsed into an empty
    // body: that would mask "too large", "too slow" and "connection dropped"
    // as a legitimate empty request and let the handler return a misleading
    // 2xx. They surface as 413 / 408 / 400 instead. The `Bytes` becomes the
    // `Vec` without a copy when it is the only owner of its buffer.
    let body_bytes = match tokio::time::timeout(
        cx.limits.body_read_timeout,
        axum::body::to_bytes(body, cx.limits.max_body_bytes),
    )
    .await
    {
        Ok(Ok(bytes)) => Vec::from(bytes),
        Ok(Err(e)) => return body_read_error_response(&e),
        Err(_elapsed) => return body_timeout_response(cx.limits.body_read_timeout),
    };

    let uri = &parts.uri;
    // SEC-07: the peer address is the `ConnectInfo` extension `serve_connection`
    // inserts on every request of the connection. Default to it; trust
    // `X-Forwarded-For` only from a configured trusted proxy so a direct
    // client cannot spoof its IP.
    let peer_ip = parts
        .extensions
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());
    let xff_lines = parts
        .headers
        .get_all("x-forwarded-for")
        .iter()
        .map(|v| v.to_str().ok());
    let remote_addr = resolve_client_ip(peer_ip, xff_lines, &cx.trusted_proxies);

    let msg = http_to_message(
        &parts.method,
        uri.path(),
        uri.query().unwrap_or(""),
        &parts.headers,
        &remote_addr,
    );
    let input = InputStream::from_bytes(body_bytes);

    let output = match &cx.target {
        DispatchTarget::Flow(fid) => cx.runtime.run(fid, msg, input).await,
        DispatchTarget::Block(name) => cx.runtime.run_block(name, msg, input).await,
    };
    wafer_output_to_response(output).await
}

/// Accept connections until `shutdown` fires, never holding more than
/// `limits.max_connections` open at once; then drain the open connections
/// for up to `limits.shutdown_grace` and abort whatever is left.
async fn serve(
    listener: TcpListener,
    app: axum::Router,
    limits: RequestLimits,
    mut shutdown: oneshot::Receiver<()>,
) {
    let slots = Arc::new(Semaphore::new(limits.max_connections));
    // Dropped when the accept loop ends; every connection task sees that as
    // the signal to finish its in-flight request and close.
    let (closing_tx, closing_rx) = watch::channel(());
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            // Reap finished connection tasks so the set holds only live ones.
            Some(_) = connections.join_next() => {}
            accepted = accept_within_cap(&listener, &slots) => match accepted {
                Ok((stream, peer, slot)) => {
                    connections.spawn(serve_connection(
                        stream,
                        peer,
                        app.clone(),
                        limits,
                        closing_rx.clone(),
                        slot,
                    ));
                }
                Err(e) => handle_accept_error(&e).await,
            },
        }
    }
    drop(listener);
    drop(closing_tx);
    let drained = tokio::time::timeout(limits.shutdown_grace, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        tracing::warn!(
            open = connections.len(),
            grace_secs = limits.shutdown_grace.as_secs(),
            "wafer-run/http-listener connections still open after shutdown_grace_secs; aborting them"
        );
        connections.shutdown().await;
    }
}

/// Take a connection slot, then accept. The slot comes first: at the cap the
/// listener stops calling accept(), so further clients wait in the kernel
/// backlog instead of each holding a task and a socket here.
async fn accept_within_cap(
    listener: &TcpListener,
    slots: &Arc<Semaphore>,
) -> std::io::Result<(TcpStream, SocketAddr, OwnedSemaphorePermit)> {
    let slot = slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| std::io::Error::other("connection slots closed"))?;
    let (stream, peer) = listener.accept().await?;
    Ok((stream, peer, slot))
}

/// A per-connection accept failure (the client reset before we got to it) is
/// routine; anything else — typically running out of file descriptors — is
/// logged and backed off for a second so the loop does not spin on it.
async fn handle_accept_error(e: &std::io::Error) {
    use std::io::ErrorKind;
    if matches!(
        e.kind(),
        ErrorKind::ConnectionRefused | ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset
    ) {
        return;
    }
    tracing::error!(error = %e, "wafer-run/http-listener accept failed; retrying in 1s");
    tokio::time::sleep(Duration::from_secs(1)).await;
}

/// A TCP stream whose writes fail with `TimedOut` once they have made no
/// progress for `timeout`, so a client that stops reading its response
/// cannot hold the connection (and its slot) open. The clock runs only while
/// a write, flush or shutdown is blocked on a full socket buffer; any
/// progress resets it, so a slow but reading client is never cut off.
struct WriteStallTimeout {
    stream: TcpStream,
    timeout: Duration,
    stalled: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl WriteStallTimeout {
    fn new(stream: TcpStream, timeout: Duration) -> Self {
        Self {
            stream,
            timeout,
            stalled: None,
        }
    }

    /// Pass a write-side result through, arming the stall clock on
    /// `Pending` and failing once it runs out.
    fn bound<T>(
        &mut self,
        cx: &mut std::task::Context<'_>,
        result: Poll<std::io::Result<T>>,
    ) -> Poll<std::io::Result<T>> {
        if result.is_ready() {
            self.stalled = None;
            return result;
        }
        let timeout = self.timeout;
        let stalled = self
            .stalled
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(timeout)));
        match stalled.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "response write made no progress within write_timeout_secs",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncRead for WriteStallTimeout {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for WriteStallTimeout {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.stream).poll_write(cx, buf);
        this.bound(cx, result)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.stream).poll_write_vectored(cx, bufs);
        this.bound(cx, result)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.stream).poll_flush(cx);
        this.bound(cx, result)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.stream).poll_shutdown(cx);
        this.bound(cx, result)
    }
}

/// Serve one HTTP/1.1 connection. The connection's slot is released when this
/// returns.
///
/// HTTP/1 only, on purpose: hyper enforces `header_read_timeout` from the
/// first byte of every request head on HTTP/1, while protocol auto-detection
/// reads the connection preface with no deadline at all.
async fn serve_connection(
    stream: TcpStream,
    peer: SocketAddr,
    app: axum::Router,
    limits: RequestLimits,
    mut closing: watch::Receiver<()>,
    _slot: OwnedSemaphorePermit,
) {
    let service = hyper::service::service_fn(move |mut req: hyper::Request<Incoming>| {
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));
        app.clone().oneshot(req.map(Body::new))
    });
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_read_timeout);
    let io = WriteStallTimeout::new(stream, limits.write_timeout);
    let conn = builder.serve_connection(TokioIo::new(io), service);
    tokio::pin!(conn);
    let result = tokio::select! {
        result = conn.as_mut() => result,
        // Any outcome — the listener stopped either way.
        _ = closing.changed() => {
            conn.as_mut().graceful_shutdown();
            conn.as_mut().await
        }
    };
    if let Err(e) = result {
        tracing::debug!(%peer, error = %e, "wafer-run/http-listener connection ended with an error");
    }
}

/// Block implementing the HTTP transport.
///
/// Singleton infrastructure block (one listener per registration). On
/// `LifecycleType::Init` it resolves and caches its [`ListenerSettings`]
/// (listen address, dispatch target, trusted proxies, request limits). The
/// actual TCP bind and HTTP/1.1 server are spawned in [`Block::bind`] once the
/// runtime hands over a `RuntimeHandle`. `LifecycleType::Stop` signals the
/// server through a `tokio::sync::oneshot` channel and returns once it has
/// drained its connections (bounded by `shutdown_grace_secs`).
///
/// The `handle` method itself only returns `OutputStream::continue_with(msg)`;
/// real request handling happens inside the spawned server task, not in the
/// block-message pipeline.
pub(crate) struct HttpListenerBlock {
    settings: OnceLock<ListenerSettings>,
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// The server task `bind` spawned; `Stop` awaits it so the listener has
    /// drained (or aborted) its connections when `Stop` returns.
    server: Mutex<Option<JoinHandle<()>>>,
}

impl Default for HttpListenerBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpListenerBlock {
    /// Construct an unconfigured listener. Its settings are filled in from
    /// [`BlockInfo`] config on the `Init` lifecycle event; the underlying TCP
    /// listener is not bound until [`Block::bind`] runs.
    pub(crate) fn new() -> Self {
        Self {
            settings: OnceLock::new(),
            shutdown_tx: Mutex::new(None),
            server: Mutex::new(None),
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
                 Larger bodies are rejected with 413 Payload Too Large. \
                 Must be at least 1.",
                &DEFAULT_MAX_BODY_BYTES.to_string(),
            )
            .name("Max Body Bytes"),
            ConfigVar::new(
                "header_read_timeout_secs",
                "Seconds a client has to send a complete request head (request \
                 line and headers); also how long an idle keep-alive \
                 connection waits for its next request. The connection is \
                 closed when it runs out. 1 to 86400.",
                &DEFAULT_HEADER_READ_TIMEOUT_SECS.to_string(),
            )
            .name("Header Read Timeout (s)"),
            ConfigVar::new(
                "body_read_timeout_secs",
                "Seconds a client has to send the whole request body after its \
                 headers. A slower body is answered with 408 Request Timeout \
                 and the connection is closed. 1 to 86400.",
                &DEFAULT_BODY_READ_TIMEOUT_SECS.to_string(),
            )
            .name("Body Read Timeout (s)"),
            ConfigVar::new(
                "max_connections",
                "Maximum concurrently open client connections. At the cap the \
                 listener stops accepting; further clients wait in the \
                 kernel's accept backlog until a connection closes. At least 1.",
                &DEFAULT_MAX_CONNECTIONS.to_string(),
            )
            .name("Max Connections"),
            ConfigVar::new(
                "write_timeout_secs",
                "Seconds a response write may make no progress (the client is \
                 not reading) before the connection is dropped. A slow client \
                 that keeps reading is not affected. 1 to 86400.",
                &DEFAULT_WRITE_TIMEOUT_SECS.to_string(),
            )
            .name("Write Timeout (s)"),
            ConfigVar::new(
                "shutdown_grace_secs",
                "Seconds a stopping listener gives open connections to finish \
                 their in-flight request; connections still open after it are \
                 aborted. Stop returns once they are gone. 1 to 86400.",
                &DEFAULT_SHUTDOWN_GRACE_SECS.to_string(),
            )
            .name("Shutdown Grace (s)"),
            ConfigVar::new(
                "trusted_proxies",
                "Comma-separated trusted reverse proxies: exact IPs (10.0.0.1, \
                 ::1) and/or CIDR ranges (10.0.0.0/8, 2001:db8::/32). \
                 X-Forwarded-For is honored (for the client IP used in rate \
                 limiting and audit) only when the direct peer matches one of \
                 these; every X-Forwarded-For line is read as one list, which \
                 is then peeled right-to-left across trusted hops. Otherwise \
                 the peer socket address is used. Empty = never trust the \
                 header (safe default for a directly-exposed listener). \
                 Invalid entries fail Init.",
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
        if event.event_type == LifecycleType::Init && self.settings.get().is_none() {
            // Resolve every setting before caching any: on error nothing is
            // set, so `bind()` refuses to start the server. An invalid
            // security or limit config must fail loud, never silently degrade.
            let settings = ListenerSettings::from_config(&BlockConfig::from_event(&event))?;
            self.settings.set(settings).ok();
        }

        if event.event_type == LifecycleType::Stop {
            if let Some(tx) = self.shutdown_tx.lock().take() {
                let _ = tx.send(());
            }
            // Bounded by `shutdown_grace_secs` inside the task.
            let server = self.server.lock().take();
            if let Some(server) = server {
                if let Err(e) = server.await {
                    tracing::error!(error = %e, "wafer-run/http-listener server task failed");
                }
            }
        }
        Ok(())
    }

    fn bind(&self, handle: Box<dyn std::any::Any + Send + Sync>) {
        let Ok(handle) = handle.downcast::<Arc<dyn wafer_block::Runtime>>() else {
            return;
        };
        let Some(settings) = self.settings.get() else {
            return;
        };
        let Some(target) = settings.target.clone() else {
            return;
        };
        if settings.listen.is_empty() {
            return;
        }
        let listen = settings.listen.clone();
        let limits = settings.limits;
        let cx = Arc::new(RequestContext {
            runtime: *handle,
            target,
            trusted_proxies: settings.trusted_proxies.clone(),
            limits,
        });

        let (tx, rx) = oneshot::channel();
        *self.shutdown_tx.lock() = Some(tx);

        let server = tokio::spawn(async move {
            let handler = axum::routing::any(move |req: Request| dispatch_request(cx.clone(), req));
            let app = axum::Router::new()
                .route("/{*rest}", handler.clone())
                .route("/", handler);

            let listener = match TcpListener::bind(&listen).await {
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

            serve(listener, app, limits, rx).await;
        });
        *self.server.lock() = Some(server);
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
mod server_tests;

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

    /// Test helper: resolve with at most one `X-Forwarded-For` line.
    fn resolve(peer: Option<IpAddr>, xff: Option<&str>, trusted: &[IpNet]) -> String {
        resolve_client_ip(peer, xff.into_iter().map(Some), trusted)
    }

    #[test]
    fn ipv4_mapped_addresses_are_compared_and_recorded_as_ipv4() {
        // A listener on `[::]` sees an IPv4 proxy as `::ffff:10.0.0.1`; it
        // must still be the trusted `10.0.0.1`, or every client collapses
        // into the proxy's address.
        let mapped_peer = Some("::ffff:10.0.0.1".parse().unwrap());
        assert_eq!(
            resolve(mapped_peer, Some("198.51.100.7"), &proxies("10.0.0.1")),
            "198.51.100.7"
        );
        assert_eq!(
            resolve(mapped_peer, Some("198.51.100.7"), &proxies("10.0.0.0/8")),
            "198.51.100.7"
        );
        // A mapped trusted entry means its IPv4 address.
        assert_eq!(
            proxies("::ffff:10.0.0.1"),
            vec!["10.0.0.1/32".parse::<IpNet>().unwrap()]
        );
        // Mapped XFF entries are peeled as trusted hops and recorded as IPv4.
        assert_eq!(
            resolve(
                mapped_peer,
                Some("::ffff:198.51.100.7, ::ffff:10.0.0.2"),
                &proxies("10.0.0.0/24")
            ),
            "198.51.100.7"
        );
        // The peer itself is recorded as IPv4 when XFF is not trusted.
        assert_eq!(resolve(mapped_peer, None, &[]), "10.0.0.1");
    }

    #[test]
    fn client_ip_reads_every_xff_line_as_one_chain() {
        // A trusted proxy that appends its hop as a separate line (HAProxy
        // `option forwardfor`): the client wrote the first line, the proxy
        // the second. The second line is the client IP.
        let trusted = proxies("10.0.0.1");
        let peer = Some("10.0.0.1".parse().unwrap());
        let lines = [Some("203.0.113.99"), Some("198.51.100.7")];
        assert_eq!(
            resolve_client_ip(peer, lines.into_iter(), &trusted),
            "198.51.100.7"
        );
        // Trusted hops are peeled across line boundaries.
        let lines = [Some("203.0.113.99, 198.51.100.7"), Some("10.0.0.2")];
        assert_eq!(
            resolve_client_ip(peer, lines.into_iter(), &proxies("10.0.0.0/24")),
            "198.51.100.7"
        );
        // A non-text line is a malformed hop: nothing left of it is trusted.
        let lines = [Some("198.51.100.7"), None];
        assert_eq!(
            resolve_client_ip(peer, lines.into_iter(), &trusted),
            "10.0.0.1"
        );
    }

    #[test]
    fn client_ip_defaults_to_peer_and_ignores_xff_from_untrusted() {
        let peer: IpAddr = "203.0.113.5".parse().unwrap();
        // No trusted proxies configured: a direct client's X-Forwarded-For is
        // ignored; the peer address wins (no spoofing).
        assert_eq!(resolve(Some(peer), Some("1.2.3.4"), &[]), "203.0.113.5");
        // Peer not in the trusted set: XFF still ignored.
        assert_eq!(
            resolve(Some(peer), Some("1.2.3.4"), &proxies("10.0.0.1")),
            "203.0.113.5"
        );
        // Peer just outside a trusted CIDR: XFF still ignored.
        assert_eq!(
            resolve(Some(peer), Some("1.2.3.4"), &proxies("203.0.113.6/31")),
            "203.0.113.5"
        );
    }

    #[test]
    fn client_ip_single_hop_from_trusted_proxy() {
        let proxy: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = proxies("10.0.0.1");
        // Peer IS the trusted proxy → the single XFF entry is the client.
        assert_eq!(resolve(Some(proxy), Some("9.9.9.9"), &trusted), "9.9.9.9");
        // Rightmost entry is not a trusted proxy → it is the client, even
        // with more (attacker-suppliable) entries to its left.
        assert_eq!(
            resolve(Some(proxy), Some("1.2.3.4, 8.8.8.8"), &trusted),
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
            resolve(
                Some("10.0.0.1".parse().unwrap()),
                Some("9.9.9.9, 10.0.0.3, 10.0.0.2"),
                &trusted
            ),
            "9.9.9.9"
        );
        // The spoof attempt "1.2.3.4" left of the real client is ignored:
        // peeling stops at the first (rightmost) untrusted entry.
        assert_eq!(
            resolve(
                Some("10.0.0.1".parse().unwrap()),
                Some("1.2.3.4, 9.9.9.9, 10.0.0.2"),
                &trusted
            ),
            "9.9.9.9"
        );
        // IPv6 client through IPv6 trusted proxies.
        assert_eq!(
            resolve(
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
            resolve(
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
            resolve(peer, Some("9.9.9.9, garbage, 10.0.0.2"), &trusted),
            "10.0.0.1"
        );
        // Rightmost entry malformed: nothing peels; fall back to peer.
        assert_eq!(
            resolve(peer, Some("9.9.9.9, not-an-ip"), &trusted),
            "10.0.0.1"
        );
        // Port-suffixed and empty segments are malformed, not lenient-parsed.
        assert_eq!(resolve(peer, Some("9.9.9.9:1234"), &trusted), "10.0.0.1");
        assert_eq!(
            resolve(peer, Some("9.9.9.9,, 10.0.0.2"), &trusted),
            "10.0.0.1"
        );
    }

    #[test]
    fn client_ip_empty_or_missing_xff_falls_back_to_peer() {
        let trusted = proxies("10.0.0.1");
        let peer: Option<IpAddr> = Some("10.0.0.1".parse().unwrap());
        assert_eq!(resolve(peer, None, &trusted), "10.0.0.1");
        assert_eq!(resolve(peer, Some(""), &trusted), "10.0.0.1");
        assert_eq!(resolve(peer, Some("   "), &trusted), "10.0.0.1");
    }

    #[test]
    fn client_ip_unknown_without_peer() {
        // No peer address at all: XFF is never consulted, even if proxies
        // are configured.
        assert_eq!(resolve(None, Some("8.8.8.8"), &[]), "unknown");
        assert_eq!(
            resolve(None, Some("8.8.8.8"), &proxies("10.0.0.1")),
            "unknown"
        );
    }

    // ── SEC-07: Init fails loud on invalid trusted_proxies config ─────────

    /// Minimal `Context` impl for driving `lifecycle()` directly; the
    /// listener's Init path never touches the context.
    pub(crate) struct NoopCtx;

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
        // Denies every access, as the trait's default `check_resource_access` does.
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_block::types::ResourceType,
            _access: wafer_block::types::ResourceAccess,
        ) -> bool {
            false
        }
    }

    pub(crate) fn init_event(config: &serde_json::Value) -> LifecycleEvent {
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
        assert!(block.settings.get().is_none());
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
            block.settings.get().expect("set at Init").trusted_proxies,
            proxies("10.0.0.0/8, ::1")
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
        assert!(block.settings.get().is_none());
    }

    /// Absent limits use the documented defaults (no error).
    #[tokio::test]
    async fn init_absent_limits_use_defaults() {
        let block = HttpListenerBlock::new();
        let event = init_event(&serde_json::json!({
            "listen": "127.0.0.1:0",
            "flow": "some-flow",
        }));
        block
            .lifecycle(&NoopCtx, event)
            .await
            .expect("absent limits must pass Init");
        assert_eq!(
            block.settings.get().expect("set at Init").limits,
            RequestLimits {
                max_body_bytes: 10 * 1024 * 1024,
                header_read_timeout: Duration::from_secs(30),
                body_read_timeout: Duration::from_secs(120),
                max_connections: 1024,
                write_timeout: Duration::from_secs(60),
                shutdown_grace: Duration::from_secs(10),
            }
        );
    }

    /// Limits arrive as JSON numbers (`add_block_config`) or as strings (flow
    /// `config_map`); both are read.
    #[tokio::test]
    async fn init_reads_limits_as_numbers_or_strings() {
        let block = HttpListenerBlock::new();
        let event = init_event(&serde_json::json!({
            "listen": "127.0.0.1:0",
            "flow": "some-flow",
            "max_body_bytes": 2048,
            "header_read_timeout_secs": "5",
            "body_read_timeout_secs": 7,
            "max_connections": "3",
            "write_timeout_secs": 11,
            "shutdown_grace_secs": "13",
        }));
        block
            .lifecycle(&NoopCtx, event)
            .await
            .expect("valid limits must pass Init");
        assert_eq!(
            block.settings.get().expect("set at Init").limits,
            RequestLimits {
                max_body_bytes: 2048,
                header_read_timeout: Duration::from_secs(5),
                body_read_timeout: Duration::from_secs(7),
                max_connections: 3,
                write_timeout: Duration::from_secs(11),
                shutdown_grace: Duration::from_secs(13),
            }
        );
    }

    /// A limit that is zero, negative, fractional, out of range or not a
    /// number fails Init naming the key, and caches nothing.
    #[tokio::test]
    async fn init_rejects_invalid_limits() {
        for (key, bad) in [
            ("max_connections", serde_json::json!(0)),
            ("max_connections", serde_json::json!("-1")),
            ("header_read_timeout_secs", serde_json::json!(1.5)),
            ("header_read_timeout_secs", serde_json::json!(86_401)),
            ("body_read_timeout_secs", serde_json::json!("soon")),
            ("body_read_timeout_secs", serde_json::json!(true)),
            ("max_body_bytes", serde_json::json!(0)),
            ("write_timeout_secs", serde_json::json!(0)),
            ("shutdown_grace_secs", serde_json::json!(86_401)),
        ] {
            let block = HttpListenerBlock::new();
            let mut config = serde_json::json!({ "listen": "127.0.0.1:0", "flow": "some-flow" });
            config[key] = bad.clone();
            let err = block
                .lifecycle(&NoopCtx, init_event(&config))
                .await
                .expect_err("invalid limit must fail Init");
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{key}={bad}");
            assert!(
                err.message.contains(key),
                "error must name {key}, got: {}",
                err.message
            );
            assert!(
                block.settings.get().is_none(),
                "{key}={bad} cached settings"
            );
        }
    }
}
