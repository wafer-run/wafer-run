use std::sync::Arc;

use wafer_block::{
    block::Block,
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::{BlockInfo, ConfigVar},
    BlockRegistry, RuntimeError, *,
};

use crate::interfaces::network::{handler, service::NetworkService};

/// Unified network block. Wraps any `NetworkService` implementation.
pub struct NetworkBlock {
    service: Arc<dyn NetworkService>,
}

impl NetworkBlock {
    /// Wrap the given `NetworkService` implementation as a `NetworkBlock`.
    pub fn new(service: Arc<dyn NetworkService>) -> Self {
        Self { service }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for NetworkBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/network",
            "0.0.1",
            "http-client@v1",
            "Outbound HTTP requests",
        )
        .category(BlockCategory::Service)
        .config_keys(vec![ConfigVar::new(
            "WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES",
            "Maximum response body size in bytes accepted by the HTTP \
             network service. Responses exceeding this limit are rejected \
             with an error rather than buffered. Defaults to 50 MiB \
             (52428800) when unset.",
            "52428800",
        )
        .name("Max Response Body Bytes")])
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        handler::handle_message(self.service.as_ref(), &msg, &body).await
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
pub fn register_with(
    w: &mut dyn BlockRegistry,
    service: Arc<dyn NetworkService>,
) -> Result<(), RuntimeError> {
    w.register_block("wafer-run/network", Arc::new(NetworkBlock::new(service)))
}
