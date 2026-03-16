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

    // Register HTTP server (infra + router)
    let port = std::env::var("PORT").unwrap_or_else(|_| "8090".to_string());
    wafer_flow_http_server::register(&mut w, serde_json::json!({
        "listen": format!("0.0.0.0:{}", port),
        "routes": [
            { "path": "/_inspector/**", "block": "wafer-run/inspector" },
            { "path": "/_inspector", "block": "wafer-run/inspector" },
            { "path": "/api/**", "block": "wafer-site/api" },
            { "path": "/playground/**", "block": "wafer-site/playground" },
            { "path": "/playground", "block": "wafer-site/playground" },
            { "path": "/registry/**", "block": "wafer-site/registry" },
            { "path": "/registry", "block": "wafer-site/registry" },
            { "path": "/docs/**", "block": "wafer-site/docs" },
            { "path": "/docs", "block": "wafer-site/docs" },
            { "path": "/**", "block": "wafer-site/docs" }
        ]
    }));
    w.add_block_config("wafer-run/logger", serde_json::json!({}));
    wafer_block_auth_validator::register(&mut w);
    wafer_block_iam_guard::register(&mut w);
    wafer_block_inspector::register(&mut w);
    wafer_block_web::register(&mut w);
    wafer_block_config::register(&mut w);
    wafer_block_logger::register(&mut w);
    wafer_block_crypto::register(&mut w);

    // Register site-specific blocks
    register_site_blocks(&mut w);
    playground::register(&mut w);
    registry::register(&mut w);

    // Start — the wafer-run/http-listener block spawns the Axum listener internally
    let w = w.start().await.unwrap_or_else(|e| {
        tracing::error!("Failed to start: {}", e);
        std::process::exit(1);
    });

    tracing::info!("Listening on 0.0.0.0:{}", port);

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await.expect("failed to listen for ctrl+c");
    tracing::info!("Shutting down...");
    w.shutdown().await;
}

fn register_site_blocks(w: &mut Wafer) {
    // Documentation block — serves HTML pages
    w.register_block_func("wafer-site/docs", |_ctx, msg| {
        let path = msg.path();

        // Serve static assets
        if path == "/images/logo.webp" {
            let bytes = include_bytes!("../public/images/logo.webp");
            return respond(msg, bytes.to_vec(), "image/webp");
        }
        if path == "/favicon.ico" {
            let bytes = include_bytes!("../public/images/favicon.ico");
            return respond(msg, bytes.to_vec(), "image/x-icon");
        }
        if path == "/css/theme.css" {
            let css = include_str!(concat!(env!("OUT_DIR"), "/content/theme.css"));
            return respond(msg, css.as_bytes().to_vec(), "text/css");
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
                return ResponseBuilder::new(msg).status(301)
                    .set_header("Location", "/docs/api-runtime")
                    .body(b"Redirecting to /docs/api-runtime".to_vec(), "text/plain");
            }
            "/docs/registry" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/registry.html")),
            "/docs/deployment" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/deployment.html")),
            _ => {
                return json_respond(
                    msg,
                    &serde_json::json!({
                        "page": "wafer-site",
                        "path": path,
                        "message": "Welcome to WAFER"
                    }),
                );
            }
        };

        respond(msg, content.as_bytes().to_vec(), "text/html")
    });

    // API block — JSON endpoints
    w.register_block_func("wafer-site/api", |_ctx, msg| {
        let path = msg.path();
        match path {
            "/api/health" => json_respond(
                msg,
                &serde_json::json!({ "status": "ok" }),
            ),
            "/api/blocks" => json_respond(
                msg,
                &serde_json::json!({
                    "blocks": [
                        {"name": "wafer-run/http-listener", "version": "0.1.0"},
                        {"name": "wafer-run/router", "version": "0.1.0"},
                        {"name": "wafer-run/security-headers", "version": "0.1.0"},
                        {"name": "wafer-run/cors", "version": "0.1.0"},
                        {"name": "wafer-run/ip-rate-limit", "version": "0.1.0"},
                        {"name": "wafer-run/readonly-guard", "version": "0.1.0"},
                        {"name": "wafer-run/monitoring", "version": "0.1.0"},
                        {"name": "wafer-run/auth-validator", "version": "0.1.0"},
                        {"name": "wafer-run/iam-guard", "version": "0.1.0"},
                        {"name": "wafer-run/web", "version": "0.1.0"}
                    ]
                }),
            ),
            _ => err_not_found(msg, &format!("API endpoint not found: {}", path)),
        }
    });
}
