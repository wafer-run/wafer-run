use std::sync::Arc;

use wafer_block::block::Block;
use wafer_block::types::BlockInfo;
use wafer_block::context::Context;
use wafer_block::*;
use wafer_block::BlockRegistry;

use crate::interfaces::network::{handler, service::NetworkService};

/// Unified network block. Wraps any `NetworkService` implementation.
pub struct NetworkBlock {
    service: Arc<dyn NetworkService>,
}

impl NetworkBlock {
    pub fn new(service: Arc<dyn NetworkService>) -> Self {
        Self { service }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for NetworkBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer-run/network".to_string(),
            version: "0.1.0".to_string(),
            interface: "network@v1".to_string(),
            summary: "Outbound HTTP requests".to_string(),
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

/// Register the unified network block with the given service.
pub fn register_with(w: &mut dyn BlockRegistry, service: Arc<dyn NetworkService>) {
    w.register_block("wafer-run/network", Arc::new(NetworkBlock::new(service)));
}
