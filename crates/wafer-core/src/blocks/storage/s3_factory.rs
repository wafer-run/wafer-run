//! Individual factory for the S3 storage block.

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
            name: "solobase/s3".to_string(),
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

pub struct S3BlockFactory;

impl BlockFactory for S3BlockFactory {
    fn create(&self, config: Option<&serde_json::Value>) -> Arc<dyn Block> {
        let bucket = super::super::env_or_config_str("STORAGE_BUCKET", config, "bucket")
            .unwrap_or_else(|| "solobase".to_string());
        let prefix = super::super::env_or_config_str("STORAGE_PREFIX", config, "prefix")
            .unwrap_or_default();
        let endpoint = super::super::env_or_config_str("STORAGE_ENDPOINT", config, "endpoint")
            .unwrap_or_default();
        let region = super::super::env_or_config_str("STORAGE_REGION", config, "region")
            .unwrap_or_else(|| "us-east-1".to_string());

        let handle = tokio::runtime::Handle::current();
        let result = tokio::task::block_in_place(|| {
            if endpoint.is_empty() {
                handle.block_on(super::s3::S3StorageService::new(&bucket, &prefix))
            } else {
                handle.block_on(super::s3::S3StorageService::with_endpoint(
                    &bucket, &prefix, &endpoint, &region,
                ))
            }
        });

        match result {
            Ok(svc) => {
                tracing::info!(bucket = %bucket, "S3 storage service initialized");
                Arc::new(super::block::StorageBlock::new(Arc::new(svc)))
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to initialize S3 storage");
                Arc::new(FailedBlock(format!("S3 storage init failed: {e}")))
            }
        }
    }

    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "solobase/s3".to_string(),
            version: "0.1.0".to_string(),
            interface: "storage@v1".to_string(),
            summary: "S3-compatible storage block".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: BlockRuntime::Native,
            requires: Vec::new(),
        }
    }
}
