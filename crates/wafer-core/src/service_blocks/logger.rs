use std::sync::Arc;

use wafer_block::{
    block::Block,
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
    BlockRegistry, RuntimeError, *,
};

use crate::interfaces::logger::{handler, service::LoggerService};

/// Unified logger block. Wraps any `LoggerService` implementation.
pub struct LoggerBlock {
    service: Arc<dyn LoggerService>,
}

impl LoggerBlock {
    /// Wrap the given `LoggerService` implementation as a `LoggerBlock`.
    pub fn new(service: Arc<dyn LoggerService>) -> Self {
        Self { service }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for LoggerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/logger",
            "0.0.1",
            "logger@v1",
            "Structured logging",
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

/// Register the unified logger block with the given service.
pub fn register_with(
    w: &mut dyn BlockRegistry,
    service: Arc<dyn LoggerService>,
) -> Result<(), RuntimeError> {
    w.register_block("wafer-run/logger", Arc::new(LoggerBlock::new(service)))
}
