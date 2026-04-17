use std::sync::Arc;

use wafer_block::{
    block::Block,
    common::ServiceOp,
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
    BlockRegistry, RuntimeError, *,
};

use crate::interfaces::vector::{
    handler,
    service::{EmbeddingService, VectorService},
};

/// Unified vector block. Wraps a `VectorService` + `EmbeddingService` pair
/// and dispatches messages to the appropriate handler based on op kind.
pub struct VectorBlock {
    vector: Arc<dyn VectorService>,
    embedding: Arc<dyn EmbeddingService>,
}

impl VectorBlock {
    pub fn new(vector: Arc<dyn VectorService>, embedding: Arc<dyn EmbeddingService>) -> Self {
        Self { vector, embedding }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for VectorBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/vector",
            "0.0.1",
            "vector@v1",
            "Vector search and embedding generation",
        )
        .category(BlockCategory::Service)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        match msg.kind.as_str() {
            ServiceOp::EMBEDDING_EMBED => {
                handler::handle_embedding_message(self.embedding.as_ref(), &msg, &body).await
            }
            _ => handler::handle_message(self.vector.as_ref(), &msg, &body).await,
        }
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

/// Register the unified vector block with the given service pair.
pub fn register_with(
    w: &mut dyn BlockRegistry,
    vector: Arc<dyn VectorService>,
    embedding: Arc<dyn EmbeddingService>,
) -> Result<(), RuntimeError> {
    w.register_block(
        "wafer-run/vector",
        Arc::new(VectorBlock::new(vector, embedding)),
    )
}
