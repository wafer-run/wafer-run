use std::sync::Arc;
use wafer_run::*;

mod playground;
mod registry;

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("wafer=info".parse().unwrap()),
        )
        .init();

    tracing::info!("Starting wafer-site...");

    // Create WAFER runtime
    let mut w = Wafer::new();

    // Configure infrastructure blocks
    w.add_block_config("wafer/database", serde_json::json!({"type": "sqlite", "path": "data/wafer-site.db"}));
    w.add_block_config("wafer/network", serde_json::json!({}));
    w.add_block_config("wafer/logger", serde_json::json!({}));

    // Register wafer-core blocks
    wafer_core::register_all(&mut w);

    // Register site-specific blocks
    register_site_blocks(&mut w);
    playground::register(&mut w);
    registry::register(&mut w);

    // Add flows
    let site_flow: FlowDef = serde_json::from_str(r#"{
        "id": "site-main",
        "summary": "Website main flow",
        "config": { "on_error": "stop", "timeout": "30s" },
        "root": {
            "flow": "http-infra",
            "next": [{
                "block": "@wafer/http-router",
                "config": {
                    "routes": [
                        { "path": "/_inspector/**", "block": "@wafer/inspector" },
                        { "path": "/_inspector", "block": "@wafer/inspector" },
                        { "path": "/api/**", "block": "@wafer-site/api" },
                        { "path": "/playground/**", "block": "@wafer-site/playground" },
                        { "path": "/playground", "block": "@wafer-site/playground" },
                        { "path": "/registry/**", "block": "@wafer-site/registry" },
                        { "path": "/registry", "block": "@wafer-site/registry" },
                        { "path": "/docs/**", "block": "@wafer-site/docs" },
                        { "path": "/docs", "block": "@wafer-site/docs" },
                        { "path": "/**", "block": "@wafer-site/docs" }
                    ]
                }
            }]
        }
    }"#).expect("invalid flow JSON");

    let _ = wafer_core::flows::register_flows(&mut w);
    w.add_flow_def(&site_flow);

    // Resolve and start (start populates introspection snapshots)
    if let Err(e) = w.resolve() {
        tracing::error!("Failed to resolve: {}", e);
        std::process::exit(1);
    }
    if let Err(e) = w.start() {
        tracing::error!("Failed to start: {}", e);
        std::process::exit(1);
    }

    let w = Arc::new(w);

    // Build axum router
    let app = wafer_core::bridge::create_router(w.clone(), "site-main");

    // Start server
    let port = std::env::var("PORT").unwrap_or_else(|_| "8090".to_string());
    let addr = format!("0.0.0.0:{}", port);
    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("bind failed");

    axum::serve(listener, app).await.expect("server failed");
}

fn register_site_blocks(w: &mut Wafer) {
    // Documentation block — serves HTML pages
    w.register_block_func("@wafer-site/docs", |_ctx, msg| {
        let path = msg.path();

        // Serve static assets
        if path == "/images/hero.webp" {
            let bytes = include_bytes!("../public/images/hero.webp");
            return respond(msg.clone(), 200, bytes.to_vec(), "image/webp");
        }
        if path == "/css/theme.css" {
            let css = include_str!(concat!(env!("OUT_DIR"), "/content/theme.css"));
            return respond(msg.clone(), 200, css.as_bytes().to_vec(), "text/css");
        }

        let content = match path {
            "/" => include_str!(concat!(env!("OUT_DIR"), "/content/index.html")),
            "/docs" | "/docs/" => include_str!(concat!(env!("OUT_DIR"), "/content/docs.html")),
            "/docs/core-concepts" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/core-concepts.html")),
            "/docs/creating-a-block" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/creating-a-block.html")),
            "/docs/running-a-block" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/running-a-block.html")),
            "/docs/cli" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/cli.html")),
            "/docs/flow-configuration" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/flow-configuration.html")),
            "/docs/built-in-blocks" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/built-in-blocks.html")),
            "/docs/services" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/services.html")),
            "/docs/http-bridge" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/http-bridge.html")),
            "/docs/wasm-blocks" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/wasm-blocks.html")),
            "/docs/api-runtime" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/api-runtime.html")),
            "/docs/api-services" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/api-services.html")),
            "/docs/api-sdk" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/api-sdk.html")),
            "/docs/api-types" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/api-types.html")),
            "/docs/api-reference" => {
                return ResponseBuilder::new(msg.clone(), 301)
                    .set_header("Location", "/docs/api-runtime")
                    .body(b"Redirecting to /docs/api-runtime".to_vec(), "text/plain");
            }
            "/docs/registry" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/registry.html")),
            "/docs/deployment" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/deployment.html")),
            _ => {
                return json_respond(
                    msg.clone(),
                    200,
                    &serde_json::json!({
                        "page": "wafer-site",
                        "path": path,
                        "message": "Welcome to WAFER"
                    }),
                );
            }
        };

        respond(msg.clone(), 200, content.as_bytes().to_vec(), "text/html")
    });

    // API block — JSON endpoints
    w.register_block_func("@wafer-site/api", |_ctx, msg| {
        let path = msg.path();
        match path {
            "/api/health" => json_respond(
                msg.clone(),
                200,
                &serde_json::json!({ "status": "ok" }),
            ),
            "/api/blocks" => json_respond(
                msg.clone(),
                200,
                &serde_json::json!({
                    "blocks": [
                        {"name": "@wafer/http-router", "version": "0.1.0"},
                        {"name": "@wafer/security-headers", "version": "0.1.0"},
                        {"name": "@wafer/cors", "version": "0.1.0"},
                        {"name": "@wafer/rate-limit", "version": "0.1.0"},
                        {"name": "@wafer/readonly-guard", "version": "0.1.0"},
                        {"name": "@wafer/monitoring", "version": "0.1.0"},
                        {"name": "@wafer/auth", "version": "0.1.0"},
                        {"name": "@wafer/iam", "version": "0.1.0"},
                        {"name": "@wafer/web", "version": "0.1.0"}
                    ]
                }),
            ),
            _ => err_not_found(msg.clone(), &format!("API endpoint not found: {}", path)),
        }
    });
}
