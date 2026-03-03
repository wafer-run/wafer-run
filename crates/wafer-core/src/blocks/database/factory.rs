//! Self-configuring BlockFactory for the database infrastructure block.

use std::collections::HashMap;
use std::sync::Arc;

use wafer_run::block::{Block, BlockInfo};
use wafer_run::manifest::{CollectionDef, collections_to_tables};
use wafer_run::registry::BlockFactory;
use wafer_run::types::InstanceMode;

/// DatabaseBlockFactory creates a DatabaseBlock from config.
///
/// Config keys:
/// - `type`: "sqlite" (default) or "postgres"
/// - `path`: SQLite file path (default: "data/solobase.db")
/// - `url`: PostgreSQL connection string
///
/// Env var overrides: `DB_TYPE`, `DB_PATH`, `DATABASE_URL`
pub struct DatabaseBlockFactory;

impl BlockFactory for DatabaseBlockFactory {
    fn create(&self, config: Option<&serde_json::Value>) -> Arc<dyn Block> {
        let db_type = super::super::env_or_config_str("DB_TYPE", config, "type")
            .unwrap_or_else(|| "sqlite".to_string());

        // Parse collections from config (includes merged contributions from `uses`)
        let tables = config
            .and_then(|c| c.get("collections"))
            .and_then(|v| serde_json::from_value::<HashMap<String, CollectionDef>>(v.clone()).ok())
            .map(|colls| collections_to_tables(&colls))
            .unwrap_or_default();

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
                        tracing::error!(path = %path, error = %e, "failed to open SQLite database");
                        panic!("failed to open SQLite database at {}: {}", path, e);
                    }
                }
            }
            #[cfg(feature = "postgres")]
            "postgres" | "postgresql" => {
                let url = super::super::env_or_config_str("DATABASE_URL", config, "url")
                    .expect("PostgreSQL requires database.url or DATABASE_URL env var");

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
                        panic!("failed to connect to PostgreSQL: {}", e);
                    }
                }
            }
            other => {
                panic!("unknown database type: {} — expected 'sqlite' or 'postgres'", other);
            }
        }
    }

    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer/database".to_string(),
            version: "0.1.0".to_string(),
            interface: "wafer.infra.database".to_string(),
            summary: "Self-configuring database block factory".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }
}
