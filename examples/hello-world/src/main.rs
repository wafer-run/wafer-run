//! Minimal wafer-run example: a single inline block that responds with "Hello, World!".
//!
//! Run with: cargo run
//! Test with: curl http://localhost:8080

use wafer_run::*;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    let mut wafer = Wafer::new();

    // Register the HTTP server (infra + router)
    wafer_core::blocks::http_server::register(&mut wafer);

    // Configure the HTTP server
    wafer.add_block_config("@wafer/http-server", serde_json::json!({
        "listen": "0.0.0.0:8080",
        "routes": [{ "path": "/**", "block": "hello" }]
    }));

    // Register a simple inline block that responds with JSON
    wafer.register_block_func("hello", |_ctx, msg| {
        json_respond(msg, &serde_json::json!({
            "message": "Hello, World!",
            "path": msg.path(),
        }))
    });

    tracing::info!("starting on http://localhost:8080");
    let wafer = wafer.start().await.expect("failed to start");

    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
}
