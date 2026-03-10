//! Individual factory for the PostgreSQL database block.

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
            name: "solobase/postgres".to_string(),
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

pub struct PostgresBlockFactory;

impl BlockFactory for PostgresBlockFactory {
    fn create(&self, config: Option<&serde_json::Value>) -> Arc<dyn Block> {
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

        let url = match super::super::env_or_config_str("DATABASE_URL", config, "url") {
            Some(u) => u,
            None => {
                let reason = "PostgreSQL requires database.url or DATABASE_URL env var".to_string();
                tracing::error!("{}", reason);
                return Arc::new(FailedBlock(reason));
            }
        };

        let handle = tokio::runtime::Handle::current();
        let result = tokio::task::block_in_place(|| {
            handle.block_on(super::postgres::PostgresDatabaseService::connect(&url))
        });

        match result {
            Ok(svc) => {
                tracing::info!("PostgreSQL database connected");
                Arc::new(super::block::DatabaseBlock::new(Arc::new(svc), tables))
            }
            Err(e) => {
                let reason = format!("failed to connect to PostgreSQL: {}", e);
                tracing::error!("{}", reason);
                Arc::new(FailedBlock(reason))
            }
        }
    }

    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "solobase/postgres".to_string(),
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
}
