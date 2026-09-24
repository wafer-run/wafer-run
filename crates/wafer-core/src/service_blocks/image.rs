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
    info_extras: |this, info| info.grants(this.service.grants()),
    handle: |this, ctx, msg, body| {
        handler::handle_message(&this.service, ctx, ImageBlock::NAME, &msg, &body).await
    },
}

#[cfg(test)]
mod tests {
    use wafer_block::{
        types::{ResourceGrant, ResourceType},
        Block,
    };

    use super::*;
    use crate::interfaces::image::router::MultiBackendImageService;

    /// The block registers under `NAME`, the namespace its resources use,
    /// and declares the grants its service returns — the router's own
    /// followed by each backend's.
    #[test]
    fn the_block_declares_its_services_grants_under_its_name() {
        let own =
            ResourceGrant::read("acme/chat", "wafer_run__image__*").typed(ResourceType::Image);
        let mut inner = MultiBackendImageService::new();
        inner.grant(ResourceGrant::read("acme/other", "wafer_run__image__b/*"));
        let mut router = MultiBackendImageService::new();
        router.grant(own).register("inner", Arc::new(inner));

        let info = ImageBlock::new(Arc::new(router)).info();
        assert_eq!(info.name, ImageBlock::NAME);
        let declared: Vec<_> = info
            .grants
            .iter()
            .map(|g| (g.grantee.as_str(), g.resource.as_str()))
            .collect();
        assert_eq!(
            declared,
            vec![
                ("acme/chat", "wafer_run__image__*"),
                ("acme/other", "wafer_run__image__b/*")
            ]
        );
    }
}
