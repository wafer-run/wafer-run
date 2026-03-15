//! PostgreSQL database block — `wafer-run/postgres`.
//!
//! Self-contained block wrapping the PostgreSQL database service.
//! Uses the shared database message handler for the `database@v1` interface.

pub mod service;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use wafer_run::block::{Block, BlockInfo};
use wafer_run::context::Context;
use wafer_run::manifest::{CollectionDef, collections_to_tables};
use wafer_run::schema::Table;
use wafer_run::types::*;

use wafer_core::interfaces::database::service::DatabaseService;
use service::PostgresDatabaseService;

/// The PostgreSQL database block.
///
/// Initialized during `lifecycle(Init)` from config (reads `DATABASE_URL`
/// env var or `url` config key). Connection is established asynchronously.
pub struct PostgresDatabaseBlock {
    service: OnceLock<Arc<dyn DatabaseService>>,
    tables: OnceLock<Vec<Table>>,
}

impl PostgresDatabaseBlock {
    pub fn new() -> Self {
        Self {
            service: OnceLock::new(),
            tables: OnceLock::new(),
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for PostgresDatabaseBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer-run/postgres".to_string(),
            version: "0.1.0".to_string(),
            interface: "database@v1".to_string(),
            summary: "PostgreSQL database block".to_string(),
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
            .expect("wafer-run/postgres: not initialized — call lifecycle(Init) first");
        wafer_core::interfaces::database::handler::handle_message(service.as_ref(), msg).await
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init && self.service.get().is_none() {
            let config = wafer_run::BlockConfig::from_event(&event);

            let tables = match config.get("collections") {
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

            let url = config.env_or("DATABASE_URL", "url")
                .ok_or_else(|| {
                    WaferError::new(
                        "config",
                        "wafer-run/postgres: requires DATABASE_URL env var or url config",
                    )
                })?;

            let svc = PostgresDatabaseService::connect(&url)
                .await
                .map_err(|e| WaferError::new("init", format!("wafer-run/postgres: {}", e)))?;
            tracing::info!("PostgreSQL database connected");
            self.service.set(Arc::new(svc)).ok();
        }

        // Run table migrations on Init
        if event.event_type == LifecycleType::Init {
            let tables = self.tables.get().map(|t| t.as_slice()).unwrap_or(&[]);
            if let Some(service) = self.service.get() {
                wafer_core::interfaces::database::handler::handle_lifecycle(
                    service.as_ref(),
                    tables,
                    &event,
                ).await?;
            }
        }

        Ok(())
    }
}

/// Register the PostgreSQL database block with the given Wafer runtime.
pub fn register(w: &mut wafer_run::Wafer) {
    w.register_block("wafer-run/postgres", Arc::new(PostgresDatabaseBlock::new()));
}
