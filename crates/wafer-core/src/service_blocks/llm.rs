use std::sync::Arc;

use crate::interfaces::llm::{handler, service::LlmService};

crate::service_block! {
    /// Unified LLM block. Wraps any `LlmService` implementation — typically a
    /// `MultiBackendLlmService` router that fans requests out across concrete
    /// backend impls (OpenAI, Anthropic, WebLLM, …).
    block: pub LlmBlock,
    name: "wafer-run/llm",
    version: "0.0.1",
    interface: "llm@v1",
    description: "LLM service (chat streaming, model listing, local-model load/unload)",
    category: Service,
    fields: { service: Arc<dyn LlmService> },
    info_extras: |this, info| info.grants(this.service.grants()),
    handle: |this, ctx, msg, body| {
        handler::handle_message(&this.service, ctx, LlmBlock::NAME, &msg, &body).await
    },
}

#[cfg(test)]
mod tests {
    use wafer_block::{
        types::{ResourceGrant, ResourceType},
        Block,
    };

    use super::*;
    use crate::interfaces::llm::router::MultiBackendLlmService;

    /// The block registers under `NAME`, the namespace its resources use,
    /// and declares the grants its service returns — the router's own
    /// followed by each backend's.
    #[test]
    fn the_block_declares_its_services_grants_under_its_name() {
        let own = ResourceGrant::read("acme/chat", "wafer_run__llm__*").typed(ResourceType::Llm);
        let mut inner = MultiBackendLlmService::new();
        inner.grant(ResourceGrant::read("acme/other", "wafer_run__llm__b/*"));
        let mut router = MultiBackendLlmService::new();
        router.grant(own).register("inner", Arc::new(inner));

        let info = LlmBlock::new(Arc::new(router)).info();
        assert_eq!(info.name, LlmBlock::NAME);
        let declared: Vec<_> = info
            .grants
            .iter()
            .map(|g| (g.grantee.as_str(), g.resource.as_str()))
            .collect();
        assert_eq!(
            declared,
            vec![
                ("acme/chat", "wafer_run__llm__*"),
                ("acme/other", "wafer_run__llm__b/*")
            ]
        );
    }
}
