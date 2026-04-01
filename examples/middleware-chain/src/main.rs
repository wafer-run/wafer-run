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

use std::sync::atomic::{AtomicU64, Ordering};
use wafer_run::*;

static REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info,wafer=debug")
        .init();

    let mut wafer = Wafer::new();

    // --- Custom HTTP server flow with extra middleware ---
    // Instead of using the standard wafer-flow-http-server, we define our own
    // flow that inserts custom middleware (request-logger, api-key-check) between
    // the standard infra blocks and the router.
    wafer.add_flow_json(r#"{
        "id": "custom-http-server",
        "name": "Custom HTTP Server",
        "version": "1.0.0",
        "description": "HTTP server with custom middleware: security headers, CORS, request logger, API key check, router",
        "steps": [
            { "id": "security-headers", "block": "wafer-run/security-headers" },
            { "id": "cors", "block": "wafer-run/cors" },
            { "id": "logger", "block": "request-logger" },
            { "id": "auth", "block": "api-key-check" },
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

    // Register the standard infra blocks that the flow references
    wafer_block_inspector::register(&mut wafer);

    // Register standard blocks needed by the flow
    // Register the standard infra blocks that the flow references
    wafer_block_http_listener::register(&mut wafer);
    wafer_block_security_headers::register(&mut wafer);
    wafer_block_cors::register(&mut wafer);
    wafer_block_router::register(&mut wafer);

    // Flow-level config (same shape as wafer-flow-http-server::register)
    wafer.add_block_config(
        "custom-http-server",
        serde_json::json!({
            "listen": "0.0.0.0:8080",
            "routes": [
                { "path": "/_inspector/**", "block": "wafer-run/inspector" },
                { "path": "/_inspector", "block": "wafer-run/inspector" },
                { "path": "/api/**", "block": "echo-handler" },
                { "path": "/stats", "block": "stats" },
                { "path": "/**", "block": "fallback" }
            ]
        }),
    );

    wafer.add_block_config(
        "wafer-run/inspector",
        serde_json::json!({
            "allow_anonymous": true
        }),
    );

    // --- Custom middleware blocks ---

    // Request logger: logs method + path, always passes through
    wafer.register_func("request-logger", |_ctx, msg| {
        let n = REQUEST_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let method = msg.action().to_string();
        let path = msg.path().to_string();
        tracing::info!(req = n, method = %method, path = %path, "incoming request");
        msg.set_meta("request.number", &n.to_string());
        msg.cont_ref()
    });

    // API key check: requires X-Api-Key header for /api/** routes
    // Skips auth for inspector and stats endpoints
    wafer.register_func("api-key-check", |_ctx, msg| {
        let path = msg.path().to_string();
        // Skip auth for non-API routes
        if !path.starts_with("/api/") {
            return msg.cont_ref();
        }
        let key = msg.header("X-Api-Key").to_string();
        if key.is_empty() {
            msg.set_meta("resp.status", "401");
            return json_respond(
                msg,
                &serde_json::json!({
                    "error": "unauthorized",
                    "message": "missing X-Api-Key header"
                }),
            );
        }
        if key != "secret123" {
            msg.set_meta("resp.status", "403");
            return json_respond(
                msg,
                &serde_json::json!({
                    "error": "forbidden",
                    "message": "invalid API key"
                }),
            );
        }
        msg.set_meta("auth.api_key_valid", "true");
        msg.cont_ref()
    });

    // --- Handler blocks ---

    // Echo handler: echoes back the request info
    wafer.register_func("echo-handler", |_ctx, msg| {
        let body: serde_json::Value = msg.decode().unwrap_or(serde_json::Value::Null);
        let req_num = msg.get_meta("request.number").to_string();
        json_respond(
            msg,
            &serde_json::json!({
                "echo": {
                    "path": msg.path(),
                    "action": msg.action(),
                    "body": body,
                    "request_number": req_num,
                },
                "message": "request passed all middleware checks!"
            }),
        )
    });

    // Stats endpoint
    wafer.register_func("stats", |_ctx, msg| {
        json_respond(
            msg,
            &serde_json::json!({
                "total_requests": REQUEST_COUNT.load(Ordering::Relaxed),
            }),
        )
    });

    // 404 fallback
    wafer.register_func("fallback", |_ctx, msg| {
        msg.set_meta("resp.status", "404");
        json_respond(
            msg,
            &serde_json::json!({
                "error": "not found",
                "endpoints": [
                    "POST /api/echo -H 'X-Api-Key: secret123'",
                    "GET /stats",
                    "GET /_inspector/ui"
                ]
            }),
        )
    });

    tracing::info!("middleware-chain example on http://localhost:8080");
    tracing::info!("  POST /api/echo -H 'X-Api-Key: secret123' -d '{{...}}'");
    tracing::info!("  GET  /stats");
    tracing::info!("  GET  /_inspector/ui");

    let wafer = wafer.start().await.expect("failed to start");
    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
}
