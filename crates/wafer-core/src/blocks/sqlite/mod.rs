//! SQLite database block — `@wafer/sqlite`.
//!
//! Self-contained block wrapping the SQLite database service.
//! Uses the shared database message handler for the `database@v1` interface.

pub mod service;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use wafer_run::block::{Block, BlockInfo};
use wafer_run::context::Context;
use wafer_run::manifest::{CollectionDef, collections_to_tables};
use wafer_run::schema::Table;
use wafer_run::types::*;

use crate::interfaces::database::service::DatabaseService;
use service::SQLiteDatabaseService;

/// The SQLite database block.
///
/// Initialized during `lifecycle(Init)` from config (reads `DB_PATH`
/// env var or `path` config key, and `collections` for table definitions).
pub struct SqliteDatabaseBlock {
    service: OnceLock<Arc<dyn DatabaseService>>,
    tables: OnceLock<Vec<Table>>,
}

impl SqliteDatabaseBlock {
    pub fn new() -> Self {
        Self {
            service: OnceLock::new(),
            tables: OnceLock::new(),
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for SqliteDatabaseBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/sqlite".to_string(),
            version: "0.1.0".to_string(),
            interface: "database@v1".to_string(),
            summary: "SQLite database block".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: BlockRuntime::Native,
            requires: Vec::new(),
        }
    }

    async fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let service = self
            .service
            .get()
            .expect("@wafer/sqlite: not initialized — call lifecycle(Init) first");
        crate::interfaces::database::handler::handle_message(service.as_ref(), msg).await
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init && self.service.get().is_none() {
            let config: Option<serde_json::Value> = if !event.data.is_empty() {
                serde_json::from_slice(&event.data).ok()
            } else {
                None
            };

            let tables = match config.as_ref().and_then(|c| c.get("collections")) {
                Some(v) => {
                    match serde_json::from_value::<HashMap<String, CollectionDef>>(v.clone()) {
                        Ok(colls) => collections_to_tables(&colls),
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                "failed to parse database collections config"
                            );
                            Vec::new()
                        }
                    }
                }
                None => Vec::new(),
            };
            self.tables.set(tables).ok();

            let path = crate::blocks::env_or_config_str("DB_PATH", config.as_ref(), "path")
                .unwrap_or_else(|| "data/solobase.db".to_string());

            if let Some(parent) = std::path::Path::new(&path).parent() {
                std::fs::create_dir_all(parent).ok();
            }

            let svc = SQLiteDatabaseService::open(&path)
                .map_err(|e| WaferError::new("init", format!("@wafer/sqlite: {}", e)))?;
            tracing::info!(path = %path, "SQLite database opened");
            self.service.set(Arc::new(svc)).ok();
        }

        // Run table migrations on Init
        if event.event_type == LifecycleType::Init {
            let tables = self.tables.get().map(|t| t.as_slice()).unwrap_or(&[]);
            if let Some(service) = self.service.get() {
                crate::interfaces::database::handler::handle_lifecycle(
                    service.as_ref(),
                    tables,
                    &event,
                ).await?;
            }
        }

        Ok(())
    }
}

/// Register the SQLite database block with the given Wafer runtime.
pub fn register(w: &mut wafer_run::Wafer) {
    w.register_block("@wafer/sqlite", Arc::new(SqliteDatabaseBlock::new()));
}
