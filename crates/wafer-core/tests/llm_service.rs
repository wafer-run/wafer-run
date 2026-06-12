//! End-to-end integration tests for the LLM service layer.
//!
//! Exercises `interfaces::llm::handler::handle_message` — the shared dispatch
//! logic used by `LlmBlock` — against a `MultiBackendLlmService` router
//! populated with a scripted fake backend. Verifies that streaming chat frames
//! are codec-encoded onto `Chunk` events, buffered ops produce a single
//! `Chunk + Complete`, unknown ops return `InvalidArgument`, and consumer
//! drop cancels the upstream service.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use futures::{stream::BoxStream, StreamExt};
use tokio_util::sync::CancellationToken;
use wafer_block::{
    codec, common::ServiceOp, core_types::Message, stream::StreamEvent, wire::llm as wire,
};
use wafer_core::interfaces::llm::{
    handler,
    router::MultiBackendLlmService,
    service::{
        ChatChunk, ChatRequest, FinishReason, LlmError, LlmService, ModelCapabilities, ModelInfo,
        ModelStatus, TokenUsage,
    },
};

/// Scriptable fake with observable cancellation + configurable stream contents.
struct ScriptedLlm {
    backend_id: String,
    chunks: Vec<ChatChunk>,
    models: Vec<ModelInfo>,
    cancel_observed: Arc<AtomicBool>,
    /// If set, `chat_stream` awaits the cancel token before yielding — used
    /// to deterministically test cancellation.
    wait_for_cancel: bool,
}

impl ScriptedLlm {
    fn new(backend_id: &str) -> Self {
        Self {
            backend_id: backend_id.into(),
            chunks: vec![],
            models: vec![],
            cancel_observed: Arc::new(AtomicBool::new(false)),
            wait_for_cancel: false,
        }
    }

    fn with_chunks(mut self, chunks: Vec<ChatChunk>) -> Self {
        self.chunks = chunks;
        self
    }

    fn with_models(mut self, models: Vec<ModelInfo>) -> Self {
        self.models = models;
        self
    }

    fn blocking_until_cancel(mut self) -> Self {
        self.wait_for_cancel = true;
        self
    }

    fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel_observed.clone()
    }
}

#[async_trait::async_trait]
impl LlmService for ScriptedLlm {
    async fn chat_stream(
        &self,
        _req: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ChatChunk, LlmError>> {
        let chunks = self.chunks.clone();
        let cancel_flag = self.cancel_observed.clone();
        if self.wait_for_cancel {
            Box::pin(async_stream::stream! {
                cancel.cancelled().await;
                cancel_flag.store(true, Ordering::SeqCst);
                yield Err(LlmError::Cancelled);
            })
        } else {
            Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)))
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(self.models.clone())
    }

    async fn status(&self, _backend_id: &str, _model_id: &str) -> Result<ModelStatus, LlmError> {
        Ok(ModelStatus::ready())
    }

    async fn unload_model(&self, _backend_id: &str, _model_id: &str) -> Result<(), LlmError> {
        Ok(())
    }

    fn claims_backend(&self, backend_id: &str) -> bool {
        backend_id == self.backend_id
    }
}

fn wire_chat_request_bytes() -> Vec<u8> {
    let req = wire::ChatRequest {
        backend_id: "test".into(),
        model: "m".into(),
        messages: vec![wire::ChatMessage {
            role: wire::ChatRole::User,
            content: wire::ChatContent::Text("hi".into()),
            tool_call_id: None,
            tool_calls: vec![],
        }],
        params: wire::ChatParams::default(),
        tools: vec![],
        extra: serde_json::Value::Null,
    };
    codec::encode(&req).unwrap()
}

fn msg(kind: &str) -> Message {
    Message {
        kind: kind.into(),
        meta: vec![],
    }
}

fn build_router(backend: ScriptedLlm) -> Arc<dyn LlmService> {
    let mut router = MultiBackendLlmService::new();
    router.register("fake", Arc::new(backend));
    Arc::new(router)
}

#[tokio::test]
async fn chat_streams_chunks_then_completes() {
    let usage = TokenUsage::new(2, 2);
    let chunks = vec![
        ChatChunk::text("hello "),
        ChatChunk::text("world"),
        ChatChunk::finish(FinishReason::Stop, Some(usage)),
    ];
    let service = build_router(ScriptedLlm::new("test").with_chunks(chunks.clone()));

    let body = wire_chat_request_bytes();
    let stream = handler::handle_message(&service, &msg(ServiceOp::LLM_CHAT), &body).await;
    let events: Vec<_> = stream.collect().await;

    // 3 Chunks + 1 Complete
    assert_eq!(events.len(), 4, "got events: {events:?}");

    let decoded: Vec<wire::ChatChunk> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Chunk(b) => Some(codec::decode(b).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(decoded.len(), 3);
    assert!(matches!(&decoded[0].delta, wire::ChunkDelta::Text(t) if t == "hello "));
    assert!(matches!(
        decoded[2].finish_reason,
        Some(wire::FinishReason::Stop)
    ));
    assert!(matches!(events.last(), Some(StreamEvent::Complete { .. })));
}

#[tokio::test]
async fn chat_invalid_body_yields_error_terminal() {
    let service = build_router(ScriptedLlm::new("test"));
    let body = b"not json".to_vec();
    let stream = handler::handle_message(&service, &msg(ServiceOp::LLM_CHAT), &body).await;
    let events: Vec<_> = stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], StreamEvent::Error(_)));
}

#[tokio::test]
async fn unknown_operation_yields_error() {
    let service = build_router(ScriptedLlm::new("test"));
    let stream = handler::handle_message(&service, &msg("llm.mystery"), &[]).await;
    let events: Vec<_> = stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], StreamEvent::Error(_)));
}

#[tokio::test]
async fn list_models_buffered_payload_is_aggregated_json() {
    let caps = ModelCapabilities {
        streaming: true,
        ..Default::default()
    };
    let models = vec![
        ModelInfo::new("test", "m1", "One").with_capabilities(caps),
        ModelInfo::new("test", "m2", "Two"),
    ];
    let service = build_router(ScriptedLlm::new("test").with_models(models.clone()));

    let stream = handler::handle_message(&service, &msg(ServiceOp::LLM_LIST_MODELS), &[]).await;
    let buffered = stream.collect_buffered().await.unwrap();
    let decoded: Vec<wire::ModelInfo> = codec::decode(&buffered.body).unwrap();
    assert_eq!(decoded.len(), models.len());
    assert_eq!(decoded[0].backend_id, models[0].backend_id);
    assert_eq!(decoded[0].model_id, models[0].model_id);
    assert_eq!(decoded[0].display_name, models[0].display_name);
    assert!(decoded[0].capabilities.streaming);
    assert_eq!(decoded[1].model_id, models[1].model_id);
}

#[tokio::test]
async fn status_dispatches_and_returns_state() {
    let service = build_router(ScriptedLlm::new("test"));
    let body = codec::encode(&wire::StatusRequest {
        backend_id: "test".into(),
        model_id: "m1".into(),
    })
    .unwrap();
    let stream = handler::handle_message(&service, &msg(ServiceOp::LLM_STATUS), &body).await;
    let buffered = stream.collect_buffered().await.unwrap();
    let decoded: wire::ModelStatus = codec::decode(&buffered.body).unwrap();
    assert!(matches!(decoded.state, wire::ModelState::Ready));
}

#[tokio::test]
async fn unload_model_returns_empty_complete() {
    let service = build_router(ScriptedLlm::new("test"));
    let body = codec::encode(&wire::UnloadModelRequest {
        backend_id: "test".into(),
        model_id: "m1".into(),
    })
    .unwrap();
    let stream = handler::handle_message(&service, &msg(ServiceOp::LLM_UNLOAD_MODEL), &body).await;
    let buffered = stream.collect_buffered().await.unwrap();
    assert!(buffered.body.is_empty());
}

#[tokio::test]
async fn dropping_chat_stream_cancels_service() {
    let scripted = ScriptedLlm::new("test").blocking_until_cancel();
    let cancel_flag = scripted.cancel_flag();
    let service = build_router(scripted);

    let body = wire_chat_request_bytes();
    let stream = handler::handle_message(&service, &msg(ServiceOp::LLM_CHAT), &body).await;
    // Consumer drop should cancel the service through the OutputStream cancel token.
    drop(stream);

    // Give the spawned producer a moment to observe cancellation.
    for _ in 0..50 {
        if cancel_flag.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("service never observed cancellation after stream drop");
}
