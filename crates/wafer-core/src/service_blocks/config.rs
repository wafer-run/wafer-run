use std::sync::Arc;

use wafer_block::block::Block;
use wafer_block::types::BlockInfo;
use wafer_block::context::Context;
use wafer_block::*;
use wafer_block::BlockRegistry;

use crate::interfaces::config::{handler, service::ConfigService};

/// Unified config block. Wraps any `ConfigService` implementation.
pub struct ConfigBlock {
    service: Arc<dyn ConfigService>,
}

impl ConfigBlock {
    pub fn new(service: Arc<dyn ConfigService>) -> Self {
        Self { service }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for ConfigBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer-run/config".to_string(),
            version: "0.1.0".to_string(),
            interface: "config@v1".to_string(),
            summary: "Configuration key-value access".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: BlockRuntime::Both,
            requires: Vec::new(),
            collections: Vec::new(),
        }
    }

    async fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        handler::handle_message(self.service.as_ref(), msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

/// Register the unified config block with the given service.
pub fn register_with(w: &mut dyn BlockRegistry, service: Arc<dyn ConfigService>) {
    w.register_block("wafer-run/config", Arc::new(ConfigBlock::new(service)));
}
