use std::sync::Arc;

use wafer_block::common::ServiceOp;

use crate::interfaces::vector::{
    handler,
    service::{EmbeddingService, VectorService},
};

crate::service_block! {
    /// Unified vector block. Wraps a `VectorService` + `EmbeddingService` pair
    /// and dispatches messages to the appropriate handler based on op kind.
    block: pub VectorBlock,
    name: "wafer-run/vector",
    version: "0.0.1",
    interface: "vector@v1",
    description: "Vector search and embedding generation",
    category: Service,
    fields: {
        vector: Arc<dyn VectorService>,
        embedding: Arc<dyn EmbeddingService>,
    },
    handle: |this, _ctx, msg, body| match msg.kind.as_str() {
        ServiceOp::EMBEDDING_EMBED | ServiceOp::EMBEDDING_COUNT_TOKENS => {
            handler::handle_embedding_message(this.embedding.as_ref(), &msg, &body).await
        }
        _ => handler::handle_message(this.vector.as_ref(), &msg, &body).await,
    },
}
