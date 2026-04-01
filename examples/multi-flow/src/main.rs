//! Multi-flow example: demonstrates multiple registered flows and the inspector.
//!
//! Registers:
//!   - `wafer-run/http-server` — the standard infra flow (via wafer-flow-http-server)
//!   - `onboarding` — a data pipeline flow (for illustration in the inspector)
//!
//! The inspector at /_inspector/ui visualizes all registered flows.
//!
//! Run with: cargo run
//! Test with:
//!   curl http://localhost:8080/greet?name=Alice
//!   curl http://localhost:8080/_inspector/flows | python3 -m json.tool
//!   curl http://localhost:8080/_inspector/flows/onboarding | python3 -m json.tool

use wafer_run::*;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info,wafer=debug")
        .init();

    let mut wafer = Wafer::new();

    // --- Standard HTTP server flow ---
    wafer_flow_http_server::register(
        &mut wafer,
        serde_json::json!({
            "listen": "0.0.0.0:8080",
            "routes": [
                { "path": "/_inspector/**", "block": "wafer-run/inspector" },
                { "path": "/_inspector", "block": "wafer-run/inspector" },
                { "path": "/greet", "block": "greeter" },
                { "path": "/health", "block": "health" },
                { "path": "/**", "block": "not-found" }
            ]
        }),
    );

    // --- Inspector (anonymous for dev) ---
    wafer_block_inspector::register(&mut wafer);
    wafer.add_block_config(
        "wafer-run/inspector",
        serde_json::json!({
            "allow_anonymous": true
        }),
    );

    // --- A second flow: onboarding pipeline (data pipeline style) ---
    // This flow shows the inspector can display multi-step flows with
    // conditional routing — even if it's not wired to HTTP serving.
    wafer.add_flow_json(r#"{
        "id": "onboarding",
        "name": "User Onboarding Pipeline",
        "version": "1.0.0",
        "description": "Multi-step user onboarding: validate, create account, send welcome email, assign role",
        "input": {
            "type": "object",
            "properties": {
                "email": { "type": "string" },
                "name": { "type": "string" },
                "plan": { "type": "string" }
            },
            "required": ["email", "name"]
        },
        "steps": [
            {
                "id": "validate",
                "block": "validate-signup",
                "input": {
                    "email": "$.input.email",
                    "name": "$.input.name"
                }
            },
            {
                "id": "create-account",
                "block": "account-creator",
                "input": {
                    "email": "$.validate.email",
                    "name": "$.validate.name"
                }
            },
            {
                "id": "assign-role",
                "block": "role-assigner",
                "input": {
                    "user_id": "$.create-account.user_id",
                    "plan": "$.input.plan"
                },
                "next": [
                    { "when": "$.input.plan == 'enterprise'", "step": "enterprise-setup" },
                    { "step": "send-welcome" }
                ]
            },
            {
                "id": "enterprise-setup",
                "block": "enterprise-provisioner",
                "input": {
                    "user_id": "$.create-account.user_id"
                }
            },
            {
                "id": "send-welcome",
                "block": "email-sender",
                "input": {
                    "to": "$.validate.email",
                    "user_id": "$.create-account.user_id",
                    "template": "welcome"
                }
            }
        ],
        "config": {
            "timeout_ms": 30000,
            "max_steps": 10,
            "on_error": "stop"
        }
    }"#).expect("valid flow JSON");

    // --- A third flow: notification pipeline ---
    wafer.add_flow_json(r#"{
        "id": "notification-dispatch",
        "name": "Notification Dispatch",
        "version": "1.0.0",
        "description": "Routes notifications to email, push, or webhook based on user preferences",
        "steps": [
            {
                "id": "load-prefs",
                "block": "preference-loader",
                "input": { "user_id": "$.input.user_id" }
            },
            {
                "id": "route",
                "block": "notification-router",
                "input": {
                    "prefs": "$.load-prefs.preferences",
                    "message": "$.input.message"
                },
                "next": [
                    { "when": "$.load-prefs.preferences.channel == 'email'", "step": "send-email" },
                    { "when": "$.load-prefs.preferences.channel == 'push'", "step": "send-push" },
                    { "step": "send-webhook" }
                ]
            },
            {
                "id": "send-email",
                "block": "email-sender",
                "input": { "to": "$.load-prefs.preferences.email", "body": "$.input.message" }
            },
            {
                "id": "send-push",
                "block": "push-sender",
                "input": { "device_token": "$.load-prefs.preferences.device_token", "body": "$.input.message" }
            },
            {
                "id": "send-webhook",
                "block": "webhook-caller",
                "input": { "url": "$.load-prefs.preferences.webhook_url", "payload": "$.input.message" }
            }
        ],
        "config": { "timeout_ms": 10000, "on_error": "stop" }
    }"#).expect("valid flow JSON");

    // --- Handler blocks (for the HTTP routes) ---
    wafer.register_func("greeter", |_ctx, msg| {
        let name = msg.query("name").to_string();
        let greeting = if name.is_empty() {
            "Hello, stranger! Try /greet?name=YourName".to_string()
        } else {
            format!("Hello, {}! Welcome to WAFER.", name)
        };
        json_respond(msg, &serde_json::json!({ "greeting": greeting }))
    });

    wafer.register_func("health", |_ctx, msg| {
        json_respond(msg, &serde_json::json!({ "status": "ok" }))
    });

    wafer.register_func("not-found", |_ctx, msg| {
        let path = msg.path().to_string();
        msg.set_meta("resp.status", "404");
        json_respond(
            msg,
            &serde_json::json!({
                "error": "not found",
                "path": path,
                "endpoints": ["/greet?name=Alice", "/health", "/_inspector/ui"]
            }),
        )
    });

    // Stub blocks referenced by the pipeline flows (needed so they register)
    for name in &[
        "validate-signup",
        "account-creator",
        "role-assigner",
        "enterprise-provisioner",
        "email-sender",
        "preference-loader",
        "notification-router",
        "push-sender",
        "webhook-caller",
    ] {
        let block_name = name.to_string();
        wafer.register_func(*name, move |_ctx, msg| {
            json_respond(
                msg,
                &serde_json::json!({
                    "stub": true,
                    "block": block_name,
                    "message": "This is a stub — implement me!"
                }),
            )
        });
    }

    tracing::info!("multi-flow example on http://localhost:8080");
    tracing::info!("  GET /greet?name=Alice");
    tracing::info!("  GET /health");
    tracing::info!("  GET /_inspector/ui     — visualize all 3 flows");

    let wafer = wafer.start().await.expect("failed to start");
    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
}
