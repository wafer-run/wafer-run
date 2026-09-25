use std::sync::Arc;

use crate::interfaces::vector::{handler, service::VectorService};

crate::service_block! {
    /// Unified vector block. Wraps any `VectorService` implementation.
    ///
    /// It stores and searches vectors; it does not embed text. Embedding is
    /// served by `embedding@v1` blocks through
    /// [`handle_embedding_message`](handler::handle_embedding_message),
    /// which callers reach by name (`clients::vector::embed`).
    block: pub VectorBlock,
    name: "wafer-run/vector",
    version: "0.0.1",
    interface: "vector@v1",
    description: "Vector index search and maintenance",
    category: Service,
    fields: { service: Arc<dyn VectorService> },
    handle: |this, ctx, msg, body| {
        handler::handle_message(this.service.as_ref(), ctx, &msg, &body).await
    },
}
