use std::sync::Arc;

use wafer_block::{
    block::Block,
    common::ServiceOp,
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
    BlockRegistry, RuntimeError, *,
};
use wafer_block_macro::wafer_async_trait;

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
    /// Wrap the given `VectorService` + `EmbeddingService` pair as a `VectorBlock`.
    pub fn new(vector: Arc<dyn VectorService>, embedding: Arc<dyn EmbeddingService>) -> Self {
        Self { vector, embedding }
    }
}

#[wafer_async_trait]
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
            ServiceOp::EMBEDDING_EMBED | ServiceOp::EMBEDDING_COUNT_TOKENS => {
                handler::handle_embedding_message(self.embedding.as_ref(), &msg, &body).await
            }
            _ => handler::handle_message(self.vector.as_ref(), &msg, &body).await,
        }
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
