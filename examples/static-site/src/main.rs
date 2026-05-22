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
        .with_env_filter("info,wafer=debug")
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

    tracing::info!("serving static files from ./public on http://localhost:8080");
    let wafer = wafer.start().await?;

    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
    Ok(())
}
