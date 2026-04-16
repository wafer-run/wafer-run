use std::sync::Arc;

use wafer_block::{
    block::Block,
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
    BlockRegistry, RuntimeError, *,
};

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
        BlockInfo::new(
            "wafer-run/config",
            "0.0.1",
            "config@v1",
            "Configuration key-value access",
        )
        .category(BlockCategory::Service)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        handler::handle_message(self.service.as_ref(), &msg, &body)
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
pub fn register_with(
    w: &mut dyn BlockRegistry,
    service: Arc<dyn ConfigService>,
) -> Result<(), RuntimeError> {
    w.register_block("wafer-run/config", Arc::new(ConfigBlock::new(service)))
}
