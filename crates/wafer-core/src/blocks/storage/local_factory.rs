//! Individual factory for the local filesystem storage block.

use std::sync::Arc;

use wafer_run::block::{Block, BlockInfo};
use wafer_run::common::ErrorCode;
use wafer_run::context::Context;
use wafer_run::registry::BlockFactory;
use wafer_run::types::*;

struct FailedBlock(String);

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for FailedBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "solobase/local-storage".to_string(),
            version: "0.1.0".to_string(),
            interface: "storage@v1".to_string(),
            summary: format!("FAILED: {}", self.0),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: BlockRuntime::Native,
            requires: Vec::new(),
        }
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: &mut Message) -> Result_ {
        Result_::error(WaferError::new(ErrorCode::UNAVAILABLE, format!("storage unavailable: {}", self.0)))
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _event: LifecycleEvent) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

pub struct LocalStorageBlockFactory;

impl BlockFactory for LocalStorageBlockFactory {
    fn create(&self, config: Option<&serde_json::Value>) -> Arc<dyn Block> {
        let root = super::super::env_or_config_str("STORAGE_ROOT", config, "root")
            .unwrap_or_else(|| "data/storage".to_string());

        match super::local::LocalStorageService::new(&root) {
            Ok(svc) => {
                tracing::info!(root = %root, "local storage service initialized");
                Arc::new(super::block::StorageBlock::new(Arc::new(svc)))
            }
            Err(e) => {
                tracing::error!(root = %root, error = %e, "failed to initialize local storage");
                Arc::new(FailedBlock(format!("local storage init failed: {e}")))
            }
        }
    }

    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "solobase/local-storage".to_string(),
            version: "0.1.0".to_string(),
            interface: "storage@v1".to_string(),
            summary: "Local filesystem storage block".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: BlockRuntime::Native,
            requires: Vec::new(),
        }
    }
}
