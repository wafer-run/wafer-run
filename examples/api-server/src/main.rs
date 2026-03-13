//! REST API server with SQLite, CORS, and security headers.
//!
//! Demonstrates using @wafer/sqlite for database, @wafer/cors for CORS,
//! and inline blocks for custom API handlers.
//!
//! Run with: cargo run
//! Test with:
//!   curl -X POST http://localhost:8080/api/notes -H 'Content-Type: application/json' -d '{"title":"Hello","body":"World"}'
//!   curl http://localhost:8080/api/notes

use std::sync::Arc;

use wafer_core::clients::database as db;
use wafer_run::*;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info,wafer=debug")
        .init();

    let mut wafer = Wafer::new();

    // --- Register blocks ---
    wafer_core::flows::http_server::register(&mut wafer, serde_json::json!({
        "listen": "0.0.0.0:8080",
        "routes": [{ "path": "/api/**", "block": "api-handler" }]
    }));
    wafer_core::blocks::sqlite::register(&mut wafer);
    wafer_core::blocks::logger::register(&mut wafer);
    wafer.register_block("api-handler", Arc::new(NotesHandler));
    wafer.add_block_config("@wafer/sqlite", serde_json::json!({
        "type": "sqlite",
        "path": "data/notes.db"
    }));
    wafer.add_block_config("@wafer/cors", serde_json::json!({
        "allow_origins": ["*"]
    }));

    // Alias @wafer/sqlite as @wafer/database (the standard database interface)
    wafer.add_alias("@wafer/database", "@wafer/sqlite");

    // Ensure data directory exists
    std::fs::create_dir_all("data").ok();

    tracing::info!("API server starting on http://localhost:8080");
    let wafer = wafer.start().await.expect("failed to start");

    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
}

// ---------------------------------------------------------------------------
// Custom block: Notes CRUD handler
// ---------------------------------------------------------------------------

struct NotesHandler;

#[async_trait::async_trait]
impl Block for NotesHandler {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "api-handler".to_string(),
            version: "0.0.1".to_string(),
            interface: "http.handler".to_string(),
            summary: "Notes CRUD API".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: vec![],
            admin_ui: None,
            runtime: BlockRuntime::Native,
            requires: vec![],
        }
    }

    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let path = msg.path().to_string();
        let action = msg.action().to_string();

        match (action.as_str(), path.as_str()) {
            // List notes
            ("retrieve", "/api/notes") => {
                let opts = db::ListOptions::default();
                match db::list(ctx, "notes", &opts).await {
                    Ok(result) => json_respond(msg, &serde_json::json!({
                        "notes": result.records,
                        "total": result.total_count,
                    })),
                    Err(e) => err_internal(msg, &e.to_string()),
                }
            }
            // Create note
            ("create", "/api/notes") => {
                let body: serde_json::Value = msg.decode().unwrap_or_default();
                let mut data = std::collections::HashMap::new();
                data.insert("title".to_string(), body.get("title").cloned().unwrap_or_default());
                data.insert("body".to_string(), body.get("body").cloned().unwrap_or_default());

                match db::create(ctx, "notes", data).await {
                    Ok(record) => json_respond(msg, &record),
                    Err(e) => err_internal(msg, &e.to_string()),
                }
            }
            // Fallback
            _ => json_respond(msg, &serde_json::json!({
                "error": "not found",
                "path": path,
                "hint": "try GET /api/notes or POST /api/notes"
            })),
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
