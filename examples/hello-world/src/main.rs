//! Minimal wafer-run example: a single inline block that responds with "Hello, World!".
//!
//! Run with: cargo run
//! Test with: curl http://localhost:8080

use std::sync::Arc;

use wafer_run::*;

struct HelloBlock;

#[async_trait::async_trait]
impl Block for HelloBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("hello", "0.0.1", "http-handler@v1", "Hello World block")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let body = serde_json::to_vec(&serde_json::json!({
            "message": "Hello, World!",
            "path": msg.path(),
        }))
        .unwrap_or_default();
        OutputStream::respond(body)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let mut wafer = Wafer::new(Arc::new(StaticConfigSource::default()))?;

    // Register the HTTP server (infra + router)
    wafer_flow_http_server::register(
        &mut wafer,
        serde_json::json!({
            "listen": "0.0.0.0:8080",
            "routes": [{ "path": "/**", "block": "example/hello" }]
        }),
    )
    .expect("register http server");

    // Register a simple inline block that responds with JSON
    wafer
        .register_block("example/hello", Arc::new(HelloBlock))
        .expect("register hello");

    tracing::info!("starting on http://localhost:8080");
    let wafer = wafer.start().await?;

    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
    Ok(())
}
