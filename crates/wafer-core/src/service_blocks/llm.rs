use std::sync::Arc;

use wafer_block::{
    block::Block,
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
    BlockRegistry, RuntimeError, *,
};
use wafer_block_macro::wafer_async_trait;

use crate::interfaces::llm::{handler, service::LlmService};

/// Unified LLM block. Wraps any `LlmService` implementation — typically a
/// `MultiBackendLlmService` router that fans requests out across concrete
/// backend impls (OpenAI, Anthropic, WebLLM, …).
pub struct LlmBlock {
    service: Arc<dyn LlmService>,
}

impl LlmBlock {
    /// Wrap the given `LlmService` implementation as an `LlmBlock`.
    pub fn new(service: Arc<dyn LlmService>) -> Self {
        Self { service }
    }
}

#[wafer_async_trait]
impl Block for LlmBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/llm",
            "0.0.1",
            "llm@v1",
            "LLM service (chat streaming, model listing, local-model load/unload)",
        )
        .category(BlockCategory::Service)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        handler::handle_message(&self.service, &msg, &body).await
    }
}

/// Register the unified LLM block under `wafer-run/llm`.
pub fn register_with(
    w: &mut dyn BlockRegistry,
    service: Arc<dyn LlmService>,
) -> Result<(), RuntimeError> {
    w.register_block("wafer-run/llm", Arc::new(LlmBlock::new(service)))
}
