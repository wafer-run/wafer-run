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
pub(crate) struct HttpListenerBlock {
    target: OnceLock<DispatchTarget>,
    listen: OnceLock<String>,
    max_body_bytes: OnceLock<usize>,
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
                        let body_bytes = axum::body::to_bytes(body, max_body_bytes)
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
                            .map_or_else(
                                || {
                                    parts.extensions.get::<std::net::SocketAddr>().map_or_else(
                                        || "unknown".to_string(),
                                        |a| a.ip().to_string(),
                                    )
                                },
                                |s| s.trim().to_string(),
                            );

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

wafer_block::register_static_block!("wafer-run/http-listener", HttpListenerBlock);

// Query-decoding semantics (`+` → space, `%XX`, invalid-sequence tolerance)
// are pinned by table-driven tests next to the single implementation in
// `wafer_block::http_codec` (the former `url_decode_tests` moved there).
