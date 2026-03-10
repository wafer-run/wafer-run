//! Individual factory for the SQLite database block.

use std::collections::HashMap;
use std::sync::Arc;

use wafer_run::block::{Block, BlockInfo};
use wafer_run::common::ErrorCode;
use wafer_run::manifest::{CollectionDef, collections_to_tables};
use wafer_run::registry::BlockFactory;
use wafer_run::types::*;

struct FailedBlock(String);

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for FailedBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "solobase/sqlite".to_string(),
            version: "0.1.0".to_string(),
            interface: "database@v1".to_string(),
            summary: format!("FAILED: {}", self.0),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: BlockRuntime::Native,
            requires: Vec::new(),
        }
    }
    async fn handle(&self, _ctx: &dyn wafer_run::context::Context, _msg: &mut Message) -> Result_ {
        Result_::error(WaferError::new(ErrorCode::INTERNAL, format!("block failed to initialize: {}", self.0)))
    }
    async fn lifecycle(&self, _ctx: &dyn wafer_run::context::Context, _event: LifecycleEvent) -> std::result::Result<(), WaferError> {
        Err(WaferError::new(ErrorCode::INTERNAL, format!("block failed to initialize: {}", self.0)))
    }
}

pub struct SqliteBlockFactory;

impl BlockFactory for SqliteBlockFactory {
    fn create(&self, config: Option<&serde_json::Value>) -> Arc<dyn Block> {
        // Parse collections from config
        let tables = match config.and_then(|c| c.get("collections")) {
            Some(v) => match serde_json::from_value::<HashMap<String, CollectionDef>>(v.clone()) {
                Ok(colls) => collections_to_tables(&colls),
                Err(e) => {
                    tracing::error!(error = %e, "failed to parse database collections config");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };

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
                Arc::new(FailedBlock(reason))
            }
        }
    }

    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "solobase/sqlite".to_string(),
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
}
