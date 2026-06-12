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

use std::sync::Arc;

// Force-link `wafer-block-inspector` so its `register_static_block!`
// inventory entry survives into the binary.
use wafer_block_inspector as _;
use wafer_run::*;

// ---------------------------------------------------------------------------
// Handler blocks
// ---------------------------------------------------------------------------

struct GreeterBlock;

#[async_trait::async_trait]
impl Block for GreeterBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("greeter", "0.0.1", "http-handler@v1", "Greeter")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let name = msg.query("name").to_string();
        let greeting = if name.is_empty() {
            "Hello, stranger! Try /greet?name=YourName".to_string()
        } else {
            format!("Hello, {name}! Welcome to WAFER.")
        };
        let body =
            serde_json::to_vec(&serde_json::json!({ "greeting": greeting })).unwrap_or_default();
        OutputStream::respond(body)
    }
}

struct HealthBlock;

#[async_trait::async_trait]
impl Block for HealthBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("health", "0.0.1", "http-handler@v1", "Health check")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let body = serde_json::to_vec(&serde_json::json!({ "status": "ok" })).unwrap_or_default();
        OutputStream::respond(body)
    }
}

struct NotFoundBlock;

#[async_trait::async_trait]
impl Block for NotFoundBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("not-found", "0.0.1", "http-handler@v1", "404 fallback")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let path = msg.path().to_string();
        let body = serde_json::to_vec(&serde_json::json!({
            "error": "not found",
            "path": path,
            "endpoints": ["/greet?name=Alice", "/health", "/_inspector/ui"]
        }))
        .unwrap_or_default();
        OutputStream::respond_with_meta(
            body,
            vec![MetaEntry {
                key: META_RESP_STATUS.to_string(),
                value: "404".to_string(),
            }],
        )
    }
}

struct StubBlock {
    name: String,
}

#[async_trait::async_trait]
impl Block for StubBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(&self.name, "0.0.1", "http-handler@v1", "Stub block")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let body = serde_json::to_vec(&serde_json::json!({
            "stub": true,
            "block": self.name,
            "message": "This is a stub — implement me!"
        }))
        .unwrap_or_default();
        OutputStream::respond(body)
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,wafer=debug")),
        )
        .init();

    let mut wafer = Wafer::new(Arc::new(StaticConfigSource::default()))?;

    // --- Standard HTTP server flow ---
    wafer
        .add_flow_json(wafer_flow_http_server::FLOW_JSON)
        .expect("register http server flow");
    wafer.add_block_config(
        wafer_flow_http_server::FLOW_ID,
        serde_json::json!({
            "listen": "0.0.0.0:8080",
            "routes": [
                { "path": "/_inspector/**", "block": "wafer-run/inspector" },
                { "path": "/_inspector", "block": "wafer-run/inspector" },
                { "path": "/greet", "block": "example/greeter" },
                { "path": "/health", "block": "example/health" },
                { "path": "/**", "block": "example/not-found" }
            ]
        }),
    );

    // --- Inspector (anonymous for dev) ---
    // wafer-run/inspector is loaded automatically by Wafer::new() via inventory autoreg.
    wafer.add_block_config(
        "wafer-run/inspector",
        serde_json::json!({
            "allow_anonymous": true
        }),
    );

    // --- A second flow: onboarding pipeline (data pipeline style) ---
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
            { "id": "validate", "block": "example/validate-signup", "input": { "email": "$.input.email", "name": "$.input.name" } },
            { "id": "create-account", "block": "example/account-creator", "input": { "email": "$.validate.email", "name": "$.validate.name" } },
            { "id": "assign-role", "block": "example/role-assigner", "input": { "user_id": "$.create-account.user_id", "plan": "$.input.plan" },
              "next": [
                { "when": "$.input.plan == 'enterprise'", "step": "enterprise-setup" },
                { "step": "send-welcome" }
              ]
            },
            { "id": "enterprise-setup", "block": "example/enterprise-provisioner", "input": { "user_id": "$.create-account.user_id" } },
            { "id": "send-welcome", "block": "example/email-sender", "input": { "to": "$.validate.email", "user_id": "$.create-account.user_id", "template": "welcome" } }
        ],
        "config": { "timeout_ms": 30000, "max_steps": 10, "on_error": "stop" }
    }"#).expect("valid flow JSON");

    // --- A third flow: notification pipeline ---
    wafer.add_flow_json(r#"{
        "id": "notification-dispatch",
        "name": "Notification Dispatch",
        "version": "1.0.0",
        "description": "Routes notifications to email, push, or webhook based on user preferences",
        "steps": [
            { "id": "load-prefs", "block": "example/preference-loader", "input": { "user_id": "$.input.user_id" } },
            { "id": "route", "block": "example/notification-router", "input": { "prefs": "$.load-prefs.preferences", "message": "$.input.message" },
              "next": [
                { "when": "$.load-prefs.preferences.channel == 'email'", "step": "send-email" },
                { "when": "$.load-prefs.preferences.channel == 'push'", "step": "send-push" },
                { "step": "send-webhook" }
              ]
            },
            { "id": "send-email", "block": "example/email-sender", "input": { "to": "$.load-prefs.preferences.email", "body": "$.input.message" } },
            { "id": "send-push", "block": "example/push-sender", "input": { "device_token": "$.load-prefs.preferences.device_token", "body": "$.input.message" } },
            { "id": "send-webhook", "block": "example/webhook-caller", "input": { "url": "$.load-prefs.preferences.webhook_url", "payload": "$.input.message" } }
        ],
        "config": { "timeout_ms": 10000, "on_error": "stop" }
    }"#).expect("valid flow JSON");

    // --- Handler blocks (for the HTTP routes) ---
    wafer
        .register_block("example/greeter", Arc::new(GreeterBlock))
        .expect("register greeter");
    wafer
        .register_block("example/health", Arc::new(HealthBlock))
        .expect("register health");
    wafer
        .register_block("example/not-found", Arc::new(NotFoundBlock))
        .expect("register not-found");

    // Stub blocks referenced by the pipeline flows
    for name in &[
        "example/validate-signup",
        "example/account-creator",
        "example/role-assigner",
        "example/enterprise-provisioner",
        "example/email-sender",
        "example/preference-loader",
        "example/notification-router",
        "example/push-sender",
        "example/webhook-caller",
    ] {
        wafer
            .register_block(
                *name,
                Arc::new(StubBlock {
                    name: name.to_string(),
                }),
            )
            .expect("register stub block");
    }

    tracing::info!("multi-flow example on http://localhost:8080");
    tracing::info!("  GET /greet?name=Alice");
    tracing::info!("  GET /health");
    tracing::info!("  GET /_inspector/ui     — visualize all 3 flows");

    let wafer = wafer.start().await?;
    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
    Ok(())
}
