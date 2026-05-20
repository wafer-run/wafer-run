#![warn(missing_docs)]
//! HTTP transport block for WAFER: binds a TCP listener, converts incoming
//! HTTP requests into WAFER [`Message`]s + [`InputStream`]s, dispatches to a
//! configured flow or block, and converts the resulting [`OutputStream`] back
//! into an HTTP response.
//!
//! Registered as the `wafer-run/http-listener` block via
//! [`wafer_run::register_static_block!`]. The only public entry point most
//! consumers need is the block name itself; the [`http_to_message`] and
//! [`wafer_output_to_response`] helpers are re-exported for embedders that
//! bypass the listener (e.g. running a WAFER flow inside an existing axum
//! router).

use std::sync::OnceLock;
use wafer_block_macro::wafer_async_trait;

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, Method, StatusCode},
};
use parking_lot::Mutex;
use wafer_run::{
    block::{Block, BlockCategory, BlockInfo},
    common::ErrorCode,
    meta::*,
    types::*,
    InputStream, OutputStream,
};

// ---------------------------------------------------------------------------
// HTTP <-> Message conversion
// ---------------------------------------------------------------------------

fn http_method_to_action(method: &Method) -> &'static str {
    match *method {
        Method::GET | Method::HEAD => RequestAction::RETRIEVE,
        Method::POST => RequestAction::CREATE,
        Method::PUT | Method::PATCH => RequestAction::UPDATE,
        Method::DELETE => RequestAction::DELETE,
        _ => RequestAction::EXECUTE,
    }
}

/// Convert an HTTP request head into a WAFER [`Message`].
///
/// The body is **not** placed on the message — it flows separately via
/// [`InputStream`]. Each header is copied to meta as
/// `http.header.{lowercased-name}`, each query parameter as both
/// `http.query.{name}` and `{META_REQ_QUERY_PREFIX}{name}` (with `+`/`%XX`
/// decoded via `form_urlencoded::parse`). Method/path/remote-addr are
/// mirrored into normalized request meta keys (`META_REQ_ACTION`,
/// `META_REQ_RESOURCE`, `META_REQ_CLIENT_IP`, `META_REQ_CONTENT_TYPE`) so
/// downstream blocks need not know the request originated over HTTP.
///
/// HTTP method maps to a WAFER `RequestAction`:
/// `GET`/`HEAD` → `RETRIEVE`, `POST` → `CREATE`, `PUT`/`PATCH` → `UPDATE`,
/// `DELETE` → `DELETE`, everything else → `EXECUTE`.
pub fn http_to_message(
    method: Method,
    uri_path: &str,
    raw_query: &str,
    headers: &HeaderMap,
    remote_addr: &str,
) -> Message {
    let mut msg = Message::new(format!("{method}:{uri_path}"));

    // HTTP-specific meta
    msg.set_meta("http.method", method.to_string());
    msg.set_meta("http.path", uri_path);
    msg.set_meta("http.raw_query", raw_query);
    msg.set_meta("http.remote_addr", remote_addr);
    msg.set_meta(
        "http.content_type",
        headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
    );
    msg.set_meta(
        "http.host",
        headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
    );

    // Normalized request meta
    msg.set_meta(META_REQ_ACTION, http_method_to_action(&method));
    msg.set_meta(META_REQ_RESOURCE, uri_path);
    msg.set_meta(META_REQ_CLIENT_IP, remote_addr);
    msg.set_meta(
        META_REQ_CONTENT_TYPE,
        headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
    );

    // Copy headers to meta (normalize names to lowercase for case-insensitive lookup)
    for (name, value) in headers {
        if let Ok(v) = value.to_str() {
            msg.set_meta(format!("http.header.{}", name.as_str().to_lowercase()), v);
        }
    }

    // Copy URL query params to meta. form_urlencoded handles both `+` → space
    // and `%XX` decoding.
    if !raw_query.is_empty() {
        for (key, value) in form_urlencoded::parse(raw_query.as_bytes()) {
            let key = key.into_owned();
            let value = value.into_owned();
            msg.set_meta(format!("http.query.{key}"), value.clone());
            msg.set_meta(format!("{META_REQ_QUERY_PREFIX}{key}"), value);
        }
    }

    msg
}

fn apply_response_meta(
    builder: axum::http::response::Builder,
    meta: &[MetaEntry],
) -> axum::http::response::Builder {
    let mut builder = builder;
    for entry in meta {
        let k = entry.key.as_str();
        let v = &entry.value;
        match k {
            k if k == META_RESP_STATUS => continue,
            k if k.starts_with(META_RESP_COOKIE_PREFIX) => {
                builder = builder.header("Set-Cookie", v);
            }
            k if k.starts_with(META_RESP_HEADER_PREFIX) => {
                let header_name = &k[META_RESP_HEADER_PREFIX.len()..];
                builder = builder.header(header_name, v);
            }
            k if k == META_RESP_CONTENT_TYPE => {
                builder = builder.header("Content-Type", v);
            }
            _ => {}
        }
    }
    builder
}

/// Map a semantic error code to an HTTP status code.
fn error_code_to_http_status(code: &ErrorCode) -> u16 {
    match code {
        ErrorCode::Ok => 200,
        ErrorCode::Cancelled => 499,
        ErrorCode::InvalidArgument => 400,
        ErrorCode::DeadlineExceeded => 504,
        ErrorCode::NotFound => 404,
        ErrorCode::AlreadyExists => 409,
        ErrorCode::PermissionDenied => 403,
        ErrorCode::ResourceExhausted => 429,
        ErrorCode::FailedPrecondition => 412,
        ErrorCode::Aborted => 409,
        ErrorCode::OutOfRange => 400,
        ErrorCode::Unimplemented => 501,
        ErrorCode::Internal => 500,
        ErrorCode::Unavailable => 503,
        ErrorCode::DataLoss => 500,
        ErrorCode::Unauthenticated => 401,
        _ => 500,
    }
}

fn get_status_code(meta: &[MetaEntry], default_code: u16) -> u16 {
    // Explicit override takes precedence
    if let Some(code) = MetaAccess::get(meta, META_RESP_STATUS) {
        if let Ok(n) = code.parse::<u16>() {
            return n;
        }
    }
    default_code
}

fn get_error_status_code(error: Option<&WaferError>, meta: &[MetaEntry]) -> u16 {
    // Explicit override in meta takes precedence
    let from_meta = get_status_code(meta, 0);
    if from_meta > 0 {
        return from_meta;
    }
    // Derive from error code
    if let Some(err) = error {
        return error_code_to_http_status(&err.code);
    }
    500
}

/// Collect a WAFER [`OutputStream`] and turn the terminal event into an
/// HTTP response.
///
/// Buffered: the full output body is read into memory before any bytes are
/// written to the wire (no streaming response). The terminal-event mapping
/// is:
///
/// - `Ok(buffered)` → `200` (or `META_RESP_STATUS` override) with the
///   collected body; meta keys prefixed with `META_RESP_HEADER_PREFIX` /
///   `META_RESP_COOKIE_PREFIX` become response headers / `Set-Cookie`
///   entries, defaulting `Content-Type: application/json`.
/// - `TerminalNotResponse::Error(WaferError)` → status derived from
///   [`ErrorCode`] (e.g. `NotFound` → 404, `PermissionDenied` → 403),
///   overridable via `META_RESP_STATUS`, body
///   `{"error": <code>, "message": <msg>}`.
/// - `TerminalNotResponse::Drop` → `204 No Content`.
/// - `TerminalNotResponse::Continue` → empty `200` (the HTTP boundary has
///   nowhere further to forward).
/// - `TerminalNotResponse::Malformed` → `500` (stream ended without a
///   terminal event; logged at `tracing::error`).
pub async fn wafer_output_to_response(output: OutputStream) -> axum::http::Response<Body> {
    use wafer_block::streams::output::TerminalNotResponse;

    match output.collect_buffered().await {
        Ok(buf) => {
            // Successful body response
            let status_code = get_status_code(&buf.meta, 200);
            let mut builder = axum::http::Response::builder()
                .status(StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK));

            builder = apply_response_meta(builder, &buf.meta);

            // Set default content-type if not set
            let has_ct = MetaAccess::contains_key(&buf.meta, META_RESP_CONTENT_TYPE);
            if !has_ct {
                builder = builder.header("Content-Type", "application/json");
            }

            builder
                .body(Body::from(buf.body))
                .unwrap_or_else(|_| internal_error_response())
        }

        Err(TerminalNotResponse::Error(err)) => {
            let err_meta = &err.meta;
            let status_code = get_error_status_code(Some(&err), err_meta);
            let mut builder = axum::http::Response::builder().status(
                StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            );

            builder = apply_response_meta(builder, err_meta);
            builder = builder.header("Content-Type", "application/json");

            let body = serde_json::json!({
                "error": err.code,
                "message": err.message,
            })
            .to_string();

            builder
                .body(Body::from(body))
                .unwrap_or_else(|_| internal_error_response())
        }

        Err(TerminalNotResponse::Drop) => {
            // HTTP 204 No Content
            axum::http::Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(Body::empty())
                .unwrap_or_else(|_| internal_error_response())
        }

        Err(TerminalNotResponse::Continue(msg)) => {
            // Continue means the flow should keep processing, but we're at the
            // top-level HTTP boundary — treat as an empty 200.
            let mut builder = axum::http::Response::builder().status(StatusCode::OK);
            builder = apply_response_meta(builder, &msg.meta);
            builder = builder.header("Content-Type", "application/json");

            builder
                .body(Body::empty())
                .unwrap_or_else(|_| internal_error_response())
        }

        Err(TerminalNotResponse::Malformed) => {
            tracing::error!("wafer-run/http-listener: stream ended without terminal event");
            internal_error_response()
        }
    }
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

// ---------------------------------------------------------------------------
// wafer-run/http-listener block
// ---------------------------------------------------------------------------

use wafer_run::config::DispatchTarget;

/// Block implementing the HTTP transport.
///
/// Singleton infrastructure block (one listener per registration). On
/// `LifecycleType::Init` it caches the `listen` socket address and the
/// `dispatch_target` (flow id or block name) from its [`BlockInfo`]
/// config. The actual TCP bind + axum server is spawned in
/// [`Block::bind`] once the runtime hands over a `RuntimeHandle`, and is
/// shut down via a `tokio::sync::oneshot` channel on
/// `LifecycleType::Stop`.
///
/// The `handle` method itself only returns `OutputStream::continue_with(msg)`;
/// real request handling happens inside the spawned axum task, not in the
/// block-message pipeline.
pub(crate) struct HttpListenerBlock {
    target: OnceLock<DispatchTarget>,
    listen: OnceLock<String>,
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
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
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
        ])
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_run::context::Context,
        msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::continue_with(msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn wafer_run::context::Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init && self.target.get().is_none() {
            let config = wafer_run::BlockConfig::from_event(&event);

            if let Some(t) = config.dispatch_target() {
                self.target.set(t).ok();
            }
            self.listen.set(config.str("listen").to_string()).ok();
        }

        if event.event_type == LifecycleType::Stop {
            if let Some(tx) = self.shutdown_tx.lock().take() {
                let _ = tx.send(());
            }
        }
        Ok(())
    }

    fn bind(&self, handle: Box<dyn std::any::Any + Send + Sync>) {
        let Ok(handle) = handle.downcast::<wafer_run::runtime::RuntimeHandle>() else {
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

        let (tx, rx) = tokio::sync::oneshot::channel();
        *self.shutdown_tx.lock() = Some(tx);

        tokio::spawn(async move {
            let handler = {
                let h = handle.clone();
                let target = target.clone();
                axum::routing::any(move |req: Request| {
                    let h = h.clone();
                    let target = target.clone();
                    async move {
                        let (parts, body) = req.into_parts();
                        const MAX_BODY_SIZE: usize = 10 * 1024 * 1024; // 10 MB
                        let body_bytes = axum::body::to_bytes(body, MAX_BODY_SIZE)
                            .await
                            .unwrap_or_default()
                            .to_vec();

                        let uri = &parts.uri;
                        let path = uri.path();
                        let query = uri.query().unwrap_or("");
                        let remote_addr = parts
                            .headers
                            .get("x-forwarded-for")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.rsplit(',').next())
                            .map(|s| s.trim().to_string())
                            .unwrap_or_else(|| {
                                parts
                                    .extensions
                                    .get::<std::net::SocketAddr>()
                                    .map(|a| a.ip().to_string())
                                    .unwrap_or_else(|| "unknown".to_string())
                            });

                        let msg = http_to_message(
                            parts.method,
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

            let serve = axum::serve(listener, app).with_graceful_shutdown(async {
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

wafer_run::register_static_block!("wafer-run/http-listener", HttpListenerBlock);

#[cfg(test)]
mod url_decode_tests {
    /// Pins the wire-equivalent semantics of the (now-removed)
    /// hand-rolled urlencoding_decode against the upstream
    /// form_urlencoded::parse. If a future refactor changes how
    /// query params are decoded, these tests should fail.
    fn decode_via_form_urlencoded(raw_query: &str) -> Vec<(String, String)> {
        form_urlencoded::parse(raw_query.as_bytes())
            .into_owned()
            .collect()
    }

    #[test]
    fn plus_decodes_to_space() {
        let out = decode_via_form_urlencoded("q=hello+world");
        assert_eq!(out, vec![("q".into(), "hello world".into())]);
    }

    #[test]
    fn percent_xx_decodes() {
        let out = decode_via_form_urlencoded("q=hello%20world");
        assert_eq!(out, vec![("q".into(), "hello world".into())]);
    }

    #[test]
    fn invalid_percent_sequence_does_not_panic() {
        let _ = decode_via_form_urlencoded("q=hello%ZZworld");
    }

    #[test]
    fn multiple_pairs_round_trip() {
        let out = decode_via_form_urlencoded("a=1&b=hello+world&c=%2Fpath");
        assert_eq!(
            out,
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "hello world".into()),
                ("c".into(), "/path".into()),
            ]
        );
    }
}
