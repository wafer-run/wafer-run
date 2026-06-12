use std::sync::Arc;

use crate::interfaces::image::{handler, service::ImageService};

crate::service_block! {
    /// Unified image block. Wraps any `ImageService` implementation — typically a
    /// `MultiBackendImageService` router that fans requests out across concrete
    /// backend impls (Stable Diffusion, DALL-E, …).
    block: pub ImageBlock,
    name: "wafer-run/image",
    version: "0.0.1",
    interface: "image@v1",
    description: "Image generation service (text-to-image; model listing + load/unload)",
    category: Service,
    fields: { service: Arc<dyn ImageService> },
    handle: |this, _ctx, msg, body| handler::handle_message(&this.service, &msg, &body).await,
}
