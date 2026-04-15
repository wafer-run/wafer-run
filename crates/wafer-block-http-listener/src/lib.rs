use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode};
use parking_lot::Mutex;

use wafer_run::block::{Block, BlockCategory, BlockInfo};
use wafer_run::common::ErrorCode;
use wafer_run::meta::*;
use wafer_run::types::*;
use wafer_run::{InputStream, OutputStream};

// ---------------------------------------------------------------------------
// HTTP <-> Message conversion
// ---------------------------------------------------------------------------

fn http_method_to_action(method: &Method) -> &'static str {
    match *method {
        Method::GET | Method::HEAD => "retrieve",
        Method::POST => "create",
        Method::PUT | Method::PATCH => "update",
        Method::DELETE => "delete",
        _ => "execute",
    }
}

/// Convert an HTTP request into a WAFER Message (no body — body flows via InputStream).
pub fn http_to_message(
    method: Method,
    uri_path: &str,
    raw_query: &str,
    headers: &HeaderMap,
    remote_addr: &str,
) -> Message {
    let mut msg = Message::new(format!("{}:{}", method, uri_path));

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

    // Copy URL query params to meta
    if !raw_query.is_empty() {
        for pair in raw_query.split('&') {
            let mut parts = pair.splitn(2, '=');
            if let (Some(key), Some(val)) = (parts.next(), parts.next()) {
                let decoded_val = urlencoding_decode(val);
                msg.set_meta(format!("http.query.{}", key), decoded_val.clone());
                msg.set_meta(format!("{}{}", META_REQ_QUERY_PREFIX, key), decoded_val);
            }
        }
    }

    msg
}

fn urlencoding_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'+' {
            bytes.push(b' ');
        } else if b == b'%' {
            let h1 = chars.next().and_then(|c| (c as char).to_digit(16));
            let h2 = chars.next().and_then(|c| (c as char).to_digit(16));
            if let (Some(h1), Some(h2)) = (h1, h2) {
                bytes.push((h1 * 16 + h2) as u8);
            }
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| s.to_string())
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
            k if k == META_RESP_STATUS || k == "http.status" => continue,
            k if k.starts_with(META_RESP_COOKIE_PREFIX)
                || k.starts_with("http.resp.set-cookie.") =>
            {
                builder = builder.header("Set-Cookie", v);
            }
            k if k.starts_with(META_RESP_HEADER_PREFIX) => {
                let header_name = &k[META_RESP_HEADER_PREFIX.len()..];
                builder = builder.header(header_name, v);
            }
            k if k.starts_with("http.resp.header.") => {
                let header_name = &k[17..];
                builder = builder.header(header_name, v);
            }
            k if k == META_RESP_CONTENT_TYPE || k == "Content-Type" => {
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
    if let Some(code) = MetaAccess::get(meta, "http.status") {
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

/// Convert a buffered OutputStream result to an HTTP response.
/// Buffered approach: collect the entire OutputStream before responding.
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
            let has_ct = MetaAccess::contains_key(&buf.meta, META_RESP_CONTENT_TYPE)
                || MetaAccess::contains_key(&buf.meta, "Content-Type");
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
    axum::http::Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::from("internal server error"))
        .unwrap()
}

// ---------------------------------------------------------------------------
// wafer-run/http-listener block
// ---------------------------------------------------------------------------

use wafer_run::config::DispatchTarget;

pub struct HttpListenerBlock {
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
    pub fn new() -> Self {
        Self {
            target: OnceLock::new(),
            listen: OnceLock::new(),
            shutdown_tx: Mutex::new(None),
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
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
        let handle = match handle.downcast::<wafer_run::runtime::RuntimeHandle>() {
            Ok(h) => *h,
            Err(_) => return,
        };
        let target = match self.target.get().cloned() {
            Some(t) => t,
            None => return,
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

            tracing::info!("wafer-run/http-listener listening on {}", listen);

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

pub fn register(w: &mut wafer_run::Wafer) -> Result<(), String> {
    w.register_block(
        "wafer-run/http-listener",
        Arc::new(HttpListenerBlock::new()),
    )
}
