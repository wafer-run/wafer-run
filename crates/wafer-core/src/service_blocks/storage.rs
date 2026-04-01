use std::sync::Arc;

use wafer_block::block::Block;
use wafer_block::context::Context;
use wafer_block::types::BlockInfo;
use wafer_block::BlockRegistry;
use wafer_block::*;

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
        BlockInfo::new(
            "wafer-run/storage",
            "0.0.1",
            "storage@v1",
            "Object storage service (files, folders, buckets)",
        )
        .category(BlockCategory::Service)
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
pub fn register_with(w: &mut dyn BlockRegistry, service: Arc<dyn StorageService>) {
    w.register_block("wafer-run/storage", Arc::new(StorageBlock::new(service)));
}
