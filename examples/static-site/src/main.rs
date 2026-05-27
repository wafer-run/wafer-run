//! Static file server with security headers and CORS.
//!
//! Uses wafer-flow-http-server with wafer-block-web for static file serving.
//!
//! Run with: cargo run
//! Test with: curl -v http://localhost:8080/

use std::sync::Arc;

// Force-link `wafer-block-web` so its `register_static_block!`
// inventory entry survives into the binary. See Wave 7 (PR #157)
// for the same pattern in `wafer-flow-http-server`.
use wafer_block_web as _;
use wafer_run::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,wafer=debug")),
        )
        .init();

    let mut wafer = Wafer::new(Arc::new(StaticConfigSource::default()))?;

    // --- Register blocks ---
    wafer
        .add_flow_json(wafer_flow_http_server::FLOW_JSON)
        .expect("register http server flow");
    wafer.add_block_config(
        wafer_flow_http_server::FLOW_ID,
        serde_json::json!({
            "listen": "0.0.0.0:8080",
            "routes": [{ "path": "/**", "block": "wafer-run/web" }]
        }),
    );
    // wafer-run/web is loaded automatically by Wafer::new() via inventory autoreg.
    wafer.add_block_config(
        "wafer-run/web",
        serde_json::json!({
            "web_root": "./public"
        }),
    );

    // Register the local-filesystem storage service that backs
    // `wafer-run/storage` (no `register_static_block!` ships with
    // wafer-block-local-storage, so consumers must wire it up
    // explicitly with a concrete `StorageService`). Rooted at the
    // CWD so the `"web_root": "./public"` config above resolves to
    // `./public/{key}` on disk.
    wafer_core::service_blocks::storage::register_with(
        &mut wafer,
        std::sync::Arc::new(
            wafer_block_local_storage::service::LocalStorageService::new(".")
                .expect("local storage root"),
        ),
    )
    .expect("register storage");

    // Create a public/ dir with a sample index.html if it doesn't exist
    let public = std::path::Path::new("public");
    if !public.exists() {
        std::fs::create_dir_all(public).ok();
        std::fs::write(
            public.join("index.html"),
            "<h1>Hello from wafer-run!</h1><p>Served with wafer-block-web</p>",
        )
        .ok();
        tracing::info!("created public/index.html");
    }

    // Example-only WRAP grant. Storage resources are file paths
    // (`{folder}/{key}`) so they don't follow the namespace convention
    // and there's no admin block in this example to declare a typed
    // Storage grant from `BlockInfo::grants`. Production code should
    // scope this to a path prefix matching `web_root` instead of "*".
    wafer.add_wrap_grants(vec![
        ResourceGrant::read("wafer-run/web", "*").typed(ResourceType::Storage)
    ]);

    tracing::info!("serving static files from ./public on http://localhost:8080");
    let wafer = wafer.start().await?;

    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
    Ok(())
}
