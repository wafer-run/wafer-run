use std::sync::Arc;

use wafer_block::common::ServiceOp;

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
    // Upload streaming: the request body arrives as a stream (header frame +
    // body frames), so it must NOT be collapsed via `collect_to_bytes`. Route
    // it to the streaming handler with the raw `InputStream` intact so a large
    // object streams into `StorageService::put_streaming`.
    stream_ingress: {
        op: ServiceOp::STORAGE_PUT_STREAMING,
        handle: |this, ctx, msg, input| {
            handler::handle_put_streaming(this.service.as_ref(), ctx, &msg, input).await
        },
    },
}
