use std::sync::Arc;

use wafer_run::block::{Block, BlockInfo};
use wafer_run::context::Context;
use wafer_run::types::*;
use wafer_run::Wafer;

use crate::interfaces::storage::{handler, service::StorageService};

/// Unified storage block. Wraps any `StorageService` implementation.
pub struct StorageBlock {
    service: Arc<dyn StorageService>,
}

impl StorageBlock {
    pub fn new(service: Arc<dyn StorageService>) -> Self {
        Self { service }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for StorageBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer-run/storage".to_string(),
            version: "0.1.0".to_string(),
            interface: "storage@v1".to_string(),
            summary: "Object storage service (files, folders, buckets)".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: BlockRuntime::Both,
            requires: Vec::new(),
            collections: Vec::new(),
        }
    }

    async fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        handler::handle_message(self.service.as_ref(), msg).await
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

/// Register the unified storage block with the given service.
pub fn register_with(w: &mut Wafer, service: Arc<dyn StorageService>) {
    w.register_block("wafer-run/storage", Arc::new(StorageBlock::new(service)));
}
