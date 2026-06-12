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
    handle: |this, _ctx, msg, body| handler::handle_message(&this.service, &msg, &body).await,
}
