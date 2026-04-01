use std::sync::Arc;

use wafer_block::block::Block;
use wafer_block::types::BlockInfo;
use wafer_block::context::Context;
use wafer_block::*;
use wafer_block::BlockRegistry;

use crate::interfaces::crypto::{handler, service::CryptoService};

/// Unified crypto block. Wraps any `CryptoService` implementation.
pub struct CryptoBlock {
    service: Arc<dyn CryptoService>,
}

impl CryptoBlock {
    pub fn new(service: Arc<dyn CryptoService>) -> Self {
        Self { service }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for CryptoBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("wafer-run/crypto", "0.0.1", "crypto@v1", "Cryptographic operations (hashing, JWT, random bytes)")
            .category(BlockCategory::Service)
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

/// Register the unified crypto block with the given service.
pub fn register_with(w: &mut dyn BlockRegistry, service: Arc<dyn CryptoService>) {
    w.register_block("wafer-run/crypto", Arc::new(CryptoBlock::new(service)));
}
