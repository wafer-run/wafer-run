//! S3-compatible storage block — `@wafer/s3`.
//!
//! Self-contained block wrapping the S3 storage service.
//! Uses the shared storage message handler for the `storage@v1` interface.

pub mod service;

use std::sync::{Arc, OnceLock};

use wafer_run::block::{Block, BlockInfo};
use wafer_run::context::Context;
use wafer_run::types::*;

use crate::interfaces::storage::service::StorageService;
use service::S3StorageService;

/// The S3-compatible storage block.
///
/// Initialized during `lifecycle(Init)` from config (reads `STORAGE_BUCKET`,
/// `STORAGE_PREFIX`, `STORAGE_ENDPOINT`, `STORAGE_REGION` env vars or config keys).
pub struct S3StorageBlock {
    service: OnceLock<Arc<dyn StorageService>>,
}

impl S3StorageBlock {
    pub fn new() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for S3StorageBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/s3".to_string(),
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

    async fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let service = self
            .service
            .get()
            .expect("@wafer/s3: not initialized — call lifecycle(Init) first");
        crate::interfaces::storage::handler::handle_message(service.as_ref(), msg).await
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init && self.service.get().is_none() {
            let config = wafer_run::BlockConfig::from_event(&event);

            let bucket = config.env_or("STORAGE_BUCKET", "bucket")
                .unwrap_or_else(|| "solobase".to_string());
            let prefix = config.env_or("STORAGE_PREFIX", "prefix")
                .unwrap_or_default();
            let endpoint = config.env_or("STORAGE_ENDPOINT", "endpoint")
                .unwrap_or_default();
            let region = config.env_or("STORAGE_REGION", "region")
                .unwrap_or_else(|| "us-east-1".to_string());

            let svc = if endpoint.is_empty() {
                S3StorageService::new(&bucket, &prefix).await
            } else {
                S3StorageService::with_endpoint(&bucket, &prefix, &endpoint, &region).await
            }
            .map_err(|e| WaferError::new("init", format!("@wafer/s3: {}", e)))?;

            tracing::info!(bucket = %bucket, "S3 storage service initialized");
            self.service.set(Arc::new(svc)).ok();
        }
        Ok(())
    }
}

/// Register the S3 storage block with the given Wafer runtime.
pub fn register(w: &mut wafer_run::Wafer) {
    w.register_block("@wafer/s3", Arc::new(S3StorageBlock::new()));
}
