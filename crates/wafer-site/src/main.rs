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

    // Default STORAGE_ROOT to the crate directory (where dist/ lives) so the
    // binary works regardless of CWD. Can be overridden via env var.
    if std::env::var("STORAGE_ROOT").is_err() {
        std::env::set_var("STORAGE_ROOT", env!("CARGO_MANIFEST_DIR"));
    }
    let port = std::env::var("PORT").unwrap_or_else(|_| "8090".to_string());

    // Register HTTP server with routes:
    //   - /_inspector    → runtime debugger
    //   - /api           → JSON API endpoints
    //   - /playground    → code editor + proxy to language playgrounds
    //   - /registry      → package registry browser + search API
    //   - /**            → static site content via wafer-run/web (from storage)
    wafer_flow_http_server::register(
        &mut w,
        serde_json::json!({
            "listen": format!("0.0.0.0:{}", port),
            "routes": [
                { "path": "/_inspector/**", "block": "wafer-run/inspector" },
                { "path": "/_inspector", "block": "wafer-run/inspector" },
                { "path": "/api/**", "block": "wafer-site/api" },
                { "path": "/playground/**", "block": "wafer-site/playground" },
                { "path": "/playground", "block": "wafer-site/playground" },
                { "path": "/registry/**", "block": "wafer-site/registry" },
                { "path": "/registry", "block": "wafer-site/registry" },
                { "path": "/**", "block": "wafer-run/web" }
            ]
        }),
    )
    .expect("register http server");

    // Block configs
    w.add_block_config("wafer-run/logger", serde_json::json!({}));
    w.add_block_config(
        "wafer-run/web",
        serde_json::json!({
            "web_root": "dist",
            "web_spa": "false",
            "web_index": "index.html"
        }),
    );

    // Register unified service blocks
    {
        use std::sync::Arc;
        let storage_root = std::env::var("STORAGE_ROOT")
            .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string());
        wafer_core::service_blocks::storage::register_with(
            &mut w,
            Arc::new(
                wafer_block_local_storage::service::LocalStorageService::new(&storage_root)
                    .expect("storage"),
            ),
        )
        .expect("register storage");
        w.add_alias("storage", "wafer-run/storage");
        wafer_core::service_blocks::config::register_with(
            &mut w,
            Arc::new(wafer_block_config::service::EnvConfigService::new()),
        )
        .expect("register config");
        wafer_core::service_blocks::logger::register_with(
            &mut w,
            Arc::new(wafer_block_logger::service::TracingLogger),
        )
        .expect("register logger");
        wafer_core::service_blocks::crypto::register_with(
            &mut w,
            Arc::new(wafer_block_crypto::service::Argon2JwtCryptoService::new(
                "wafer-site-dev-secret".to_string(),
            )),
        )
        .expect("register crypto");
    }

    // Register infrastructure blocks
    wafer_block_auth_validator::register(&mut w).expect("register auth-validator");
    wafer_block_iam_guard::register(&mut w).expect("register iam-guard");
    wafer_block_inspector::register(&mut w).expect("register inspector");
    w.add_block_config(
        "wafer-run/inspector",
        serde_json::json!({
            "allow_anonymous": true
        }),
    );
    wafer_block_web::register(&mut w).expect("register web");

    // Register site-specific blocks
    register_api_block(&mut w).expect("register api block");
    playground::register(&mut w).expect("register playground");
    registry::register(&mut w).expect("register registry");

    // Start — the wafer-run/http-listener block spawns the Axum listener internally
    let w = w.start().await.unwrap_or_else(|e| {
        tracing::error!("Failed to start: {}", e);
        std::process::exit(1);
    });

    tracing::info!("Listening on 0.0.0.0:{}", port);

    // Wait for shutdown signal
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl+c");
    tracing::info!("Shutting down...");
    w.shutdown().await;
}

struct ApiBlock;

#[async_trait::async_trait]
impl Block for ApiBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("wafer-site/api", "0.0.1", "http-handler@v1", "Site API")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let path = msg.path().to_string();
        match path.as_str() {
            "/api/health" => {
                let body =
                    serde_json::to_vec(&serde_json::json!({ "status": "ok" })).unwrap_or_default();
                OutputStream::respond(body)
            }
            "/api/blocks" => {
                let body = serde_json::to_vec(&serde_json::json!({
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
                        {"name": "wafer-run/web", "version": "0.2.0"}
                    ]
                }))
                .unwrap_or_default();
                OutputStream::respond(body)
            }
            _ => OutputStream::error(WaferError {
                code: ErrorCode::NotFound,
                message: format!("API endpoint not found: {}", path),
                meta: vec![],
            }),
        }
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

fn register_api_block(w: &mut Wafer) -> Result<(), String> {
    w.register_block("wafer-site/api", Arc::new(ApiBlock))
}
