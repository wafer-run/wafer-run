//! REST API server with SQLite, CORS, and security headers.
//!
//! Demonstrates using wafer-block-sqlite for database, CORS via the HTTP server flow,
//! and inline blocks for custom API handlers.
//!
//! Run with: cargo run
//! Test with:
//!   curl -X POST http://localhost:8080/api/notes -H 'Content-Type: application/json' -d '{"title":"Hello","body":"World"}'
//!   curl http://localhost:8080/api/notes

use std::sync::Arc;

use wafer_block::db::ListOptions;
use wafer_core::clients::database as db;
use wafer_run::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info,wafer=debug")
        .init();

    let mut wafer = Wafer::new(Arc::new(StaticConfigSource::default()))?;

    // --- Register blocks ---
    wafer_flow_http_server::register(
        &mut wafer,
        serde_json::json!({
            "listen": "0.0.0.0:8080",
            "routes": [
                { "path": "/_inspector/**", "block": "wafer-run/inspector" },
                { "path": "/_inspector", "block": "wafer-run/inspector" },
                { "path": "/api/**", "block": "example/api-handler" }
            ]
        }),
    )
    .expect("register http server");
    // Ensure data directory exists
    std::fs::create_dir_all("data").ok();

    // Register unified service blocks
    wafer_core::service_blocks::database::register_with(
        &mut wafer,
        Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open("data/notes.db")
                .expect("open db"),
        ),
    )
    .expect("register database");
    wafer_core::service_blocks::logger::register_with(
        &mut wafer,
        Arc::new(wafer_block_logger::service::TracingLogger),
    )
    .expect("register logger");
    // wafer-run/inspector is loaded automatically by Wafer::new() via inventory autoreg.
    wafer.add_block_config(
        "wafer-run/inspector",
        serde_json::json!({
            "allow_anonymous": true
        }),
    );
    wafer
        .register_block("example/api-handler", Arc::new(NotesHandler))
        .expect("register api-handler");
    wafer.add_block_config(
        "wafer-run/cors",
        serde_json::json!({
            "allow_origins": ["*"]
        }),
    );

    tracing::info!("API server starting on http://localhost:8080");
    let wafer = wafer.start().await?;

    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Custom block: Notes CRUD handler
// ---------------------------------------------------------------------------

struct NotesHandler;

#[async_trait::async_trait]
impl Block for NotesHandler {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("api-handler", "0.0.1", "http-handler@v1", "Notes CRUD API")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let path = msg.path().to_string();
        let action = msg.action().to_string();

        match (action.as_str(), path.as_str()) {
            // List notes
            ("retrieve", "/api/notes") => {
                let opts = ListOptions::default();
                match db::list(ctx, "notes", &opts).await {
                    Ok(result) => {
                        let body = serde_json::to_vec(&serde_json::json!({
                            "notes": result.records,
                            "total": result.total_count,
                        }))
                        .unwrap_or_default();
                        OutputStream::respond(body)
                    }
                    Err(e) => OutputStream::error(WaferError {
                        code: ErrorCode::Internal,
                        message: e.to_string(),
                        meta: vec![],
                    }),
                }
            }
            // Create note
            ("create", "/api/notes") => {
                let body_bytes = input.collect_to_bytes().await;
                let body: serde_json::Value =
                    serde_json::from_slice(&body_bytes).unwrap_or_default();
                let mut data = std::collections::HashMap::new();
                data.insert(
                    "title".to_string(),
                    body.get("title").cloned().unwrap_or_default(),
                );
                data.insert(
                    "body".to_string(),
                    body.get("body").cloned().unwrap_or_default(),
                );

                match db::create(ctx, "notes", data).await {
                    Ok(record) => {
                        let resp = serde_json::to_vec(&record).unwrap_or_default();
                        OutputStream::respond(resp)
                    }
                    Err(e) => OutputStream::error(WaferError {
                        code: ErrorCode::Internal,
                        message: e.to_string(),
                        meta: vec![],
                    }),
                }
            }
            // Fallback
            _ => {
                let body = serde_json::to_vec(&serde_json::json!({
                    "error": "not found",
                    "path": path,
                    "hint": "try GET /api/notes or POST /api/notes"
                }))
                .unwrap_or_default();
                OutputStream::respond(body)
            }
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
