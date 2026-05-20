use std::sync::Arc;

use wafer_block::{
    block::Block,
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
    BlockRegistry, RuntimeError, *,
};
use wafer_block_macro::wafer_async_trait;

use crate::interfaces::image::{handler, service::ImageService};

/// Unified image block. Wraps any `ImageService` implementation — typically a
/// `MultiBackendImageService` router that fans requests out across concrete
/// backend impls (Stable Diffusion, DALL-E, …).
pub struct ImageBlock {
    service: Arc<dyn ImageService>,
}

impl ImageBlock {
    /// Wrap the given `ImageService` implementation as an `ImageBlock`.
    pub fn new(service: Arc<dyn ImageService>) -> Self {
        Self { service }
    }
}

#[wafer_async_trait]
impl Block for ImageBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/image",
            "0.0.1",
            "image@v1",
            "Image generation service (text-to-image; model listing + load/unload)",
        )
        .category(BlockCategory::Service)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        handler::handle_message(self.service.clone(), &msg, body).await
    }
}

/// Register the unified image block under `wafer-run/image`.
pub fn register_with(
    w: &mut dyn BlockRegistry,
    service: Arc<dyn ImageService>,
) -> Result<(), RuntimeError> {
    w.register_block("wafer-run/image", Arc::new(ImageBlock::new(service)))
}
