//! Local filesystem storage block — `@wafer/local-storage`.
//!
//! Self-contained block wrapping the local filesystem storage service.
//! Uses the shared storage message handler for the `storage@v1` interface.

pub mod service;

use std::sync::{Arc, OnceLock};

use wafer_run::block::{Block, BlockInfo};
use wafer_run::context::Context;
use wafer_run::types::*;

use crate::interfaces::storage::service::StorageService;
use service::LocalStorageService;

/// The local filesystem storage block.
///
/// Initialized during `lifecycle(Init)` from config (reads `STORAGE_ROOT`
/// env var or `root` config key).
pub struct LocalStorageBlock {
    service: OnceLock<Arc<dyn StorageService>>,
}

impl LocalStorageBlock {
    pub fn new() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for LocalStorageBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/local-storage".to_string(),
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

    async fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let service = self
            .service
            .get()
            .expect("@wafer/local-storage: not initialized — call lifecycle(Init) first");
        crate::interfaces::storage::handler::handle_message(service.as_ref(), msg).await
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init && self.service.get().is_none() {
            let config = wafer_run::BlockConfig::from_event(&event);

            let root = config.env_or("STORAGE_ROOT", "root")
                .unwrap_or_else(|| "data/storage".to_string());

            let svc = LocalStorageService::new(&root)
                .map_err(|e| WaferError::new("init", format!("@wafer/local-storage: {}", e)))?;
            tracing::info!(root = %root, "local storage service initialized");
            self.service.set(Arc::new(svc)).ok();
        }
        Ok(())
    }
}

/// Register the local storage block with the given Wafer runtime.
pub fn register(w: &mut wafer_run::Wafer) {
    w.register_block("@wafer/local-storage", Arc::new(LocalStorageBlock::new()));
}
