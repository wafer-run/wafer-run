use std::sync::Arc;
use wafer_run::*;

mod playground;
mod registry;
mod services;

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

    // Register platform services (database + network)
    let platform_services = services::build_platform_services();
    w.register_platform_services(platform_services);

    // Register wafer-core blocks
    wafer_core::register_all(&mut w);

    // Register site-specific blocks
    register_site_blocks(&mut w);
    playground::register(&mut w);
    registry::register(&mut w);

    // Add chains
    let site_chain: ChainDef = serde_json::from_str(r#"{
        "id": "site-main",
        "summary": "Website main chain",
        "config": { "on_error": "stop", "timeout": "30s" },
        "http": {
            "routes": [
                { "path": "/", "path_prefix": true }
            ]
        },
        "root": {
            "chain": "http-infra",
            "next": [
                {
                    "match": "*:/api/**",
                    "block": "@wafer-site/api"
                },
                {
                    "match": "*:/playground/**",
                    "block": "@wafer-site/playground"
                },
                {
                    "match": "*:/playground",
                    "block": "@wafer-site/playground"
                },
                {
                    "match": "*:/registry/**",
                    "block": "@wafer-site/registry"
                },
                {
                    "match": "*:/registry",
                    "block": "@wafer-site/registry"
                },
                {
                    "match": "*:/docs/**",
                    "block": "@wafer-site/docs"
                },
                {
                    "match": "*:/docs",
                    "block": "@wafer-site/docs"
                },
                {
                    "block": "@wafer-site/docs"
                }
            ]
        }
    }"#).expect("invalid chain JSON");

    wafer_core::chains::register_chains(&mut w);
    w.add_chain_def(&site_chain);

    // Resolve
    if let Err(e) = w.resolve() {
        tracing::error!("Failed to resolve: {}", e);
        std::process::exit(1);
    }

    let w = Arc::new(w);

    // Build axum router
    let app = wafer_run::bridge::auto_register(w.clone());

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
            "/docs/chain-configuration" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/chain-configuration.html")),
            "/docs/built-in-blocks" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/built-in-blocks.html")),
            "/docs/services" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/services.html")),
            "/docs/http-bridge" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/http-bridge.html")),
            "/docs/wasm-blocks" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/wasm-blocks.html")),
            "/docs/api-reference" => include_str!(concat!(env!("OUT_DIR"), "/content/docs/api-reference.html")),
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
