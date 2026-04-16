//! Middleware chain example: custom request processing pipeline.
//!
//! Demonstrates building a custom flow that extends the standard HTTP server
//! flow with additional middleware steps. The chain is:
//!
//!   security-headers → cors → request-logger → api-key-check → router → handler
//!
//! Run with: cargo run
//! Test with:
//!   curl http://localhost:8080/api/echo -H "X-Api-Key: secret123" -H "Content-Type: application/json" -d '{"hello":"world"}'
//!   curl http://localhost:8080/api/echo  # blocked by api-key-check middleware
//!   curl http://localhost:8080/stats     # also blocked
//!   curl http://localhost:8080/_inspector/ui

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use wafer_run::*;

static REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Custom middleware blocks
// ---------------------------------------------------------------------------

/// Request logger: logs method + path, always passes through.
struct RequestLoggerBlock;

#[async_trait::async_trait]
impl Block for RequestLoggerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("request-logger", "0.0.1", "middleware@v1", "Request logger")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let n = REQUEST_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let method = msg.action().to_string();
        let path = msg.path().to_string();
        tracing::info!(req = n, method = %method, path = %path, "incoming request");
        let mut out_msg = msg;
        out_msg.set_meta("request.number", n.to_string());
        OutputStream::continue_with(out_msg)
    }
}

/// API key check: requires X-Api-Key header for /api/** routes.
struct ApiKeyCheckBlock;

#[async_trait::async_trait]
impl Block for ApiKeyCheckBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("api-key-check", "0.0.1", "middleware@v1", "API key check")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let path = msg.path().to_string();
        // Skip auth for non-API routes
        if !path.starts_with("/api/") {
            return OutputStream::continue_with(msg);
        }
        let key = msg.header("X-Api-Key").to_string();
        if key.is_empty() {
            return OutputStream::error(WaferError {
                code: ErrorCode::Unauthenticated,
                message: "missing X-Api-Key header".to_string(),
                meta: vec![MetaEntry {
                    key: "resp.status".into(),
                    value: "401".into(),
                }],
            });
        }
        if key != "secret123" {
            return OutputStream::error(WaferError {
                code: ErrorCode::PermissionDenied,
                message: "invalid API key".to_string(),
                meta: vec![MetaEntry {
                    key: "resp.status".into(),
                    value: "403".into(),
                }],
            });
        }
        let mut out_msg = msg;
        out_msg.set_meta("auth.api_key_valid", "true");
        OutputStream::continue_with(out_msg)
    }
}

// ---------------------------------------------------------------------------
// Handler blocks
// ---------------------------------------------------------------------------

/// Echo handler: echoes back the request info.
struct EchoHandlerBlock;

#[async_trait::async_trait]
impl Block for EchoHandlerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("echo-handler", "0.0.1", "http-handler@v1", "Echo handler")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body_bytes = input.collect_to_bytes().await;
        let body: serde_json::Value =
            serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
        let req_num = msg.get_meta("request.number").to_string();
        let resp = serde_json::to_vec(&serde_json::json!({
            "echo": {
                "path": msg.path(),
                "action": msg.action(),
                "body": body,
                "request_number": req_num,
            },
            "message": "request passed all middleware checks!"
        }))
        .unwrap_or_default();
        OutputStream::respond(resp)
    }
}

/// Stats endpoint.
struct StatsBlock;

#[async_trait::async_trait]
impl Block for StatsBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("stats", "0.0.1", "http-handler@v1", "Stats endpoint")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let body = serde_json::to_vec(&serde_json::json!({
            "total_requests": REQUEST_COUNT.load(Ordering::Relaxed),
        }))
        .unwrap_or_default();
        OutputStream::respond(body)
    }
}

/// 404 fallback.
struct FallbackBlock;

#[async_trait::async_trait]
impl Block for FallbackBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("fallback", "0.0.1", "http-handler@v1", "404 fallback")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let path = msg.path().to_string();
        OutputStream::error(WaferError {
            code: ErrorCode::NotFound,
            message: format!("path '{path}' not found"),
            meta: vec![MetaEntry {
                key: "resp.status".into(),
                value: "404".into(),
            }],
        })
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info,wafer=debug")
        .init();

    let mut wafer = Wafer::new();

    // --- Custom HTTP server flow with extra middleware ---
    wafer.add_flow_json(r#"{
        "id": "custom-http-server",
        "name": "Custom HTTP Server",
        "version": "1.0.0",
        "description": "HTTP server with custom middleware: security headers, CORS, request logger, API key check, router",
        "steps": [
            { "id": "security-headers", "block": "wafer-run/security-headers" },
            { "id": "cors", "block": "wafer-run/cors" },
            { "id": "logger", "block": "example/request-logger" },
            { "id": "auth", "block": "example/api-key-check" },
            { "id": "router", "block": "wafer-run/router" }
        ],
        "config": { "on_error": "stop" },
        "blocks": [
            "wafer-run/security-headers",
            "wafer-run/cors",
            "wafer-run/router",
            "wafer-run/http-listener"
        ],
        "config_map": {
            "listen": { "target": "wafer-run/http-listener", "key": "listen" },
            "routes": { "target": "wafer-run/router", "key": "routes" }
        },
        "config_defaults": {
            "wafer-run/http-listener": { "flow": "custom-http-server" }
        }
    }"#).expect("valid flow JSON");

    // Register standard infra blocks
    wafer_block_inspector::register(&mut wafer).expect("register inspector");
    wafer_block_http_listener::register(&mut wafer).expect("register http-listener");
    wafer_block_security_headers::register(&mut wafer).expect("register security-headers");
    wafer_block_cors::register(&mut wafer).expect("register cors");
    wafer_block_router::register(&mut wafer).expect("register router");

    // Flow-level config
    wafer.add_block_config(
        "custom-http-server",
        serde_json::json!({
            "listen": "0.0.0.0:8080",
            "routes": [
                { "path": "/_inspector/**", "block": "wafer-run/inspector" },
                { "path": "/_inspector", "block": "wafer-run/inspector" },
                { "path": "/api/**", "block": "example/echo-handler" },
                { "path": "/stats", "block": "example/stats" },
                { "path": "/**", "block": "example/fallback" }
            ]
        }),
    );

    wafer.add_block_config(
        "wafer-run/inspector",
        serde_json::json!({ "allow_anonymous": true }),
    );

    // Register custom blocks
    wafer
        .register_block("example/request-logger", Arc::new(RequestLoggerBlock))
        .expect("register request-logger");
    wafer
        .register_block("example/api-key-check", Arc::new(ApiKeyCheckBlock))
        .expect("register api-key-check");
    wafer
        .register_block("example/echo-handler", Arc::new(EchoHandlerBlock))
        .expect("register echo-handler");
    wafer
        .register_block("example/stats", Arc::new(StatsBlock))
        .expect("register stats");
    wafer
        .register_block("example/fallback", Arc::new(FallbackBlock))
        .expect("register fallback");

    tracing::info!("middleware-chain example on http://localhost:8080");
    tracing::info!("  POST /api/echo -H 'X-Api-Key: secret123' -d '{{...}}'");
    tracing::info!("  GET  /stats");
    tracing::info!("  GET  /_inspector/ui");

    let wafer = wafer.start().await.expect("failed to start");
    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
}
