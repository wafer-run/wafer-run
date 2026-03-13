//! Static file server with security headers and CORS.
//!
//! Uses @wafer/http-server with @wafer/web for static file serving.
//!
//! Run with: cargo run
//! Test with: curl -v http://localhost:8080/

use wafer_run::*;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    let mut wafer = Wafer::new();

    // --- Register blocks ---
    wafer_core::flows::http_server::register(&mut wafer, serde_json::json!({
        "listen": "0.0.0.0:8080",
        "routes": [{ "path": "/**", "block": "@wafer/web" }]
    }));
    wafer_core::blocks::web::register(&mut wafer);
    wafer.add_block_config("@wafer/web", serde_json::json!({
        "web_root": "./public"
    }));

    // Create a public/ dir with a sample index.html if it doesn't exist
    let public = std::path::Path::new("public");
    if !public.exists() {
        std::fs::create_dir_all(public).ok();
        std::fs::write(
            public.join("index.html"),
            "<h1>Hello from wafer-run!</h1><p>Served with @wafer/web</p>",
        ).ok();
        tracing::info!("created public/index.html");
    }

    tracing::info!("serving static files from ./public on http://localhost:8080");
    let wafer = wafer.start().await.expect("failed to start");

    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
}
