//! Self-configuring BlockFactory implementation for the storage block.
//!
//! Reads config from the JSON value passed to `create()`, constructs the
//! appropriate storage service backend, and wraps it in a StorageBlock.

use std::sync::Arc;

use wafer_run::block::{Block, BlockInfo};
use wafer_run::common::ErrorCode;
use wafer_run::context::Context;
use wafer_run::registry::BlockFactory;
use wafer_run::types::*;

/// Stub block returned when storage initialization fails.
struct FailedStorageBlock(String);

impl Block for FailedStorageBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer/storage".to_string(),
            version: "0.1.0".to_string(),
            interface: "wafer.infra.storage".to_string(),
            summary: format!("FAILED: {}", self.0),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }

    fn handle(&self, _ctx: &dyn Context, _msg: &mut Message) -> Result_ {
        Result_::error(WaferError::new(
            ErrorCode::UNAVAILABLE,
            format!("storage unavailable: {}", self.0),
        ))
    }

    fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

/// StorageBlockFactory creates a StorageBlock from config.
///
/// Config keys:
/// - `type`: "local" (default) or "s3"
/// - `root`: local storage root (default: "data/storage")
/// - `bucket`, `region`, `endpoint`, `prefix`: S3 config
///
/// Env var overrides: `STORAGE_TYPE`, `STORAGE_ROOT`, `STORAGE_BUCKET`, etc.
pub struct StorageBlockFactory;

impl BlockFactory for StorageBlockFactory {
    fn create(&self, config: Option<&serde_json::Value>) -> Arc<dyn Block> {
        let storage_type = super::super::env_or_config_str("STORAGE_TYPE", config, "type")
            .unwrap_or_else(|| "local".to_string());

        match storage_type.as_str() {
            #[cfg(feature = "storage-local")]
            "local" => {
                let root = super::super::env_or_config_str("STORAGE_ROOT", config, "root")
                    .unwrap_or_else(|| "data/storage".to_string());

                match super::local::LocalStorageService::new(&root) {
                    Ok(svc) => {
                        tracing::info!(root = %root, "local storage service initialized");
                        Arc::new(super::block::StorageBlock::new(Arc::new(svc)))
                    }
                    Err(e) => {
                        tracing::error!(root = %root, error = %e, "failed to initialize local storage — using error stub");
                        Arc::new(FailedStorageBlock(format!("local storage init failed: {e}")))
                    }
                }
            }
            #[cfg(feature = "storage-s3")]
            "s3" => {
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
                        handle.block_on(
                            super::s3::S3StorageService::new(&bucket, &prefix),
                        )
                    } else {
                        handle.block_on(
                            super::s3::S3StorageService::with_endpoint(
                                &bucket, &prefix, &endpoint, &region,
                            ),
                        )
                    }
                });

                match result {
                    Ok(svc) => {
                        tracing::info!(bucket = %bucket, "S3 storage service initialized");
                        Arc::new(super::block::StorageBlock::new(Arc::new(svc)))
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to initialize S3 storage — using error stub");
                        Arc::new(FailedStorageBlock(format!("S3 storage init failed: {e}")))
                    }
                }
            }
            other => {
                tracing::error!(storage_type = %other, "unknown storage type — using error stub");
                Arc::new(FailedStorageBlock(format!("unknown storage type: {other}")))
            }
        }
    }

    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer/storage".to_string(),
            version: "0.1.0".to_string(),
            interface: "wafer.infra.storage".to_string(),
            summary: "Self-configuring storage block factory".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }
}
