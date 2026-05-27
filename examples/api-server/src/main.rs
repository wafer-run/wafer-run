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
// Force-link `wafer-block-inspector` so its `register_static_block!`
// inventory entry survives into the binary. The `wafer-block-sqlite`
// and `wafer-block-logger` deps don't need this — they're explicitly
// referenced by the `register_with` calls below.
use wafer_block_inspector as _;
use wafer_core::clients::database as db;
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
            "routes": [
                { "path": "/_inspector/**", "block": "wafer-run/inspector" },
                { "path": "/_inspector", "block": "wafer-run/inspector" },
                { "path": "/api/**", "block": "example/api-handler" }
            ]
        }),
    );
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
            "allowed_origins": ["*"]
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
        BlockInfo::new(
            "example/api-handler",
            "0.0.1",
            "http-handler@v1",
            "Notes CRUD API",
        )
        .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let path = msg.path().to_string();
        let action = msg.action().to_string();

        match (action.as_str(), path.as_str()) {
            // List notes
            ("retrieve", "/api/notes") => {
                let opts = ListOptions::default();
                match db::list(ctx, "example__api_handler__notes", &opts).await {
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

                match db::create(ctx, "example__api_handler__notes", data).await {
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
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            // Ensure the namespaced table exists. `db::ddl` routes through
            // the `__ddl__` WRAP resource which any attributable caller may
            // use — no admin block required. The table name follows the
            // {org}__{block}__{name} convention so WRAP Rule 3 (own resource)
            // admits subsequent CRUD calls without a grant declaration.
            db::ddl(
                ctx,
                "CREATE TABLE IF NOT EXISTS \"example__api_handler__notes\" \
                 (id TEXT PRIMARY KEY, title TEXT, body TEXT, \
                 created_at TEXT, updated_at TEXT)",
            )
            .await
            .map_err(|e| {
                WaferError::new(ErrorCode::Internal, format!("schema migration failed: {e}"))
            })?;
        }
        Ok(())
    }
}
