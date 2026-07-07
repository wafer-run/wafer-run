use std::sync::Arc;

use crate::interfaces::storage::{handler, service::StorageService};

crate::service_block! {
    /// Unified storage block. Wraps any `StorageService` implementation.
    block: pub StorageBlock,
    name: "wafer-run/storage",
    version: "0.0.1",
    interface: "storage@v1",
    description: "Object storage service (files, folders, buckets)",
    category: Service,
    fields: { service: Arc<dyn StorageService> },
    handle: |this, ctx, msg, body| {
        handler::handle_message(this.service.as_ref(), ctx, &msg, &body).await
    },
}
