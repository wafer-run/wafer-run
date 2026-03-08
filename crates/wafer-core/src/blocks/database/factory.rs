//! BlockFactory for the database block.

use std::collections::HashMap;
use std::sync::Arc;

use wafer_run::block::{Block, BlockInfo};
use wafer_run::common::ErrorCode;
use wafer_run::manifest::{CollectionDef, collections_to_tables};
use wafer_run::registry::BlockFactory;
use wafer_run::types::*;

/// DatabaseBlockFactory creates a DatabaseBlock from config.
///
/// Config keys:
/// - `type`: "sqlite" (default) or "postgres"
/// - `path`: SQLite file path (default: "data/solobase.db")
/// - `url`: PostgreSQL connection string
///
/// Env var overrides: `DB_TYPE`, `DB_PATH`, `DATABASE_URL`
/// A stub block returned when the real database block cannot be created.
/// Every call returns an INTERNAL error describing the original failure.
struct FailedDatabaseBlock {
    reason: String,
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for FailedDatabaseBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/database".to_string(),
            version: "0.1.0".to_string(),
            interface: "database@v1".to_string(),
            summary: "Failed database block (see logs)".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Native,
            requires: Vec::new(),
        }
    }

    async fn handle(&self, _ctx: &dyn wafer_run::context::Context, _msg: &mut Message) -> Result_ {
        Result_::error(WaferError::new(
            ErrorCode::INTERNAL,
            format!("database block failed to initialize: {}", self.reason),
        ))
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn wafer_run::context::Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Err(WaferError::new(
            ErrorCode::INTERNAL,
            format!("database block failed to initialize: {}", self.reason),
        ))
    }
}

pub struct DatabaseBlockFactory;

impl BlockFactory for DatabaseBlockFactory {
    fn create(&self, config: Option<&serde_json::Value>) -> Arc<dyn Block> {
        let db_type = super::super::env_or_config_str("DB_TYPE", config, "type")
            .unwrap_or_else(|| "sqlite".to_string());

        // Parse collections from config (includes merged contributions from `uses`)
        let tables = match config.and_then(|c| c.get("collections")) {
            Some(v) => match serde_json::from_value::<HashMap<String, CollectionDef>>(v.clone()) {
                Ok(colls) => collections_to_tables(&colls),
                Err(e) => {
                    tracing::error!(error = %e, "failed to parse database collections config — no tables will be created");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };

        match db_type.as_str() {
            #[cfg(feature = "sqlite")]
            "sqlite" => {
                let path = super::super::env_or_config_str("DB_PATH", config, "path")
                    .unwrap_or_else(|| "data/solobase.db".to_string());

                if let Some(parent) = std::path::Path::new(&path).parent() {
                    std::fs::create_dir_all(parent).ok();
                }

                match super::sqlite::SQLiteDatabaseService::open(&path) {
                    Ok(svc) => {
                        tracing::info!(path = %path, "SQLite database opened");
                        Arc::new(super::block::DatabaseBlock::new(Arc::new(svc), tables))
                    }
                    Err(e) => {
                        let reason = format!("failed to open SQLite at {}: {}", path, e);
                        tracing::error!(path = %path, error = %e, "failed to open SQLite database");
                        Arc::new(FailedDatabaseBlock { reason })
                    }
                }
            }
            #[cfg(feature = "postgres")]
            "postgres" | "postgresql" => {
                let url = match super::super::env_or_config_str("DATABASE_URL", config, "url") {
                    Some(u) => u,
                    None => {
                        let reason = "PostgreSQL requires database.url or DATABASE_URL env var".to_string();
                        tracing::error!("{}", reason);
                        return Arc::new(FailedDatabaseBlock { reason });
                    }
                };

                let handle = tokio::runtime::Handle::current();
                let result = tokio::task::block_in_place(|| {
                    handle.block_on(
                        super::postgres::PostgresDatabaseService::connect(&url),
                    )
                });

                match result {
                    Ok(svc) => {
                        tracing::info!("PostgreSQL database connected");
                        Arc::new(super::block::DatabaseBlock::new(Arc::new(svc), tables))
                    }
                    Err(e) => {
                        let reason = format!("failed to connect to PostgreSQL: {}", e);
                        tracing::error!("{}", reason);
                        Arc::new(FailedDatabaseBlock { reason })
                    }
                }
            }
            other => {
                let reason = format!("unknown database type: {} — expected 'sqlite' or 'postgres'", other);
                tracing::error!("{}", reason);
                Arc::new(FailedDatabaseBlock { reason })
            }
        }
    }

    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/database".to_string(),
            version: "0.1.0".to_string(),
            interface: "database@v1".to_string(),
            summary: "Database block factory".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Native,
            requires: Vec::new(),
        }
    }
}
