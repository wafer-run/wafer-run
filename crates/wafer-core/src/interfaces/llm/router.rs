//! `MultiBackendLlmService` — dispatches chat/model operations across
//! registered `LlmService` impls by `backend_id`.
//!
//! Concrete LlmService impls (ProviderLlmService, BrowserLlmService, etc.)
//! are registered on the router with a human-readable label. The router
//! iterates registered impls and picks the first whose `claims_backend()`
//! returns true for the request's `backend_id`. `list_models` aggregates
//! across every registered impl, tolerating per-backend failures.

use std::sync::Arc;

use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;
use wafer_block_macro::wafer_async_trait;

use super::service::{
    ChatChunk, ChatRequest, LlmError, LlmService, LoadProgress, ModelInfo, ModelStatus,
};

/// `LlmService` that fans `claims_backend`-routed calls out to a list of
/// registered backend implementations.
pub struct MultiBackendLlmService {
    impls: Vec<(String, Arc<dyn LlmService>)>,
}

impl MultiBackendLlmService {
    /// Construct an empty router; backends are added with [`register`](Self::register).
    pub fn new() -> Self {
        Self { impls: Vec::new() }
    }

    /// Register an LlmService impl under a label. Later impls lose to earlier
    /// ones when more than one claims the same `backend_id`.
    pub fn register(
        &mut self,
        label: impl Into<String>,
        service: Arc<dyn LlmService>,
    ) -> &mut Self {
        self.impls.push((label.into(), service));
        self
    }

    fn find(&self, backend_id: &str) -> Option<&Arc<dyn LlmService>> {
        self.impls
            .iter()
            .find(|(_, svc)| svc.claims_backend(backend_id))
            .map(|(_, svc)| svc)
    }
}

impl Default for MultiBackendLlmService {
    fn default() -> Self {
        Self::new()
    }
}

#[wafer_async_trait]
impl LlmService for MultiBackendLlmService {
    async fn chat_stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ChatChunk, LlmError>> {
        match self.find(&req.backend_id) {
            Some(svc) => svc.chat_stream(req, cancel).await,
            None => {
                let id = req.backend_id.clone();
                Box::pin(futures::stream::once(async move {
                    Err(LlmError::InvalidRequest(format!(
                        "unknown backend_id: {id}"
                    )))
                }))
            }
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        let mut all = Vec::new();
        for (label, svc) in &self.impls {
            match svc.list_models().await {
                Ok(models) => all.extend(models),
                Err(e) => {
                    // One backend failing doesn't fail the aggregation —
                    // a configured but temporarily-down provider shouldn't
                    // blank out the admin's model picker.
                    tracing::warn!(backend = %label, error = %e, "list_models failed");
                }
            }
        }
        Ok(all)
    }

    async fn status(&self, backend_id: &str, model_id: &str) -> Result<ModelStatus, LlmError> {
        self.find(backend_id)
            .ok_or_else(|| LlmError::InvalidRequest(format!("unknown backend: {backend_id}")))?
            .status(backend_id, model_id)
            .await
    }

    fn load_model(
        &self,
        backend_id: &str,
        model_id: &str,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<LoadProgress, LlmError>> {
        #[expect(
            clippy::option_if_let_else,
            reason = "None arm is multi-statement (owns backend_id into an async stream); match reads clearer than map_or_else"
        )]
        match self.find(backend_id) {
            Some(svc) => svc.load_model(backend_id, model_id, cancel),
            None => {
                let id = backend_id.to_string();
                Box::pin(futures::stream::once(async move {
                    Err(LlmError::InvalidRequest(format!("unknown backend: {id}")))
                }))
            }
        }
    }

    async fn unload_model(&self, backend_id: &str, model_id: &str) -> Result<(), LlmError> {
        self.find(backend_id)
            .ok_or_else(|| LlmError::InvalidRequest(format!("unknown backend: {backend_id}")))?
            .unload_model(backend_id, model_id)
            .await
    }

    fn claims_backend(&self, backend_id: &str) -> bool {
        self.find(backend_id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::{
        super::service::{ChatChunk, ChatMessage, ChatRole, ChunkDelta, ModelInfo, ModelStatus},
        *,
    };
    use crate::interfaces::llm::service::{ChatContent, ModelState};

    /// Configurable fake for driving router tests.
    ///
    /// - `claims`: which backend_ids this fake handles
    /// - `text`: text delta returned by `chat_stream`
    /// - `models`: what `list_models` returns; `err_list_models` forces an error
    #[derive(Default)]
    struct FakeLlmService {
        claims: Vec<String>,
        text: Option<String>,
        models: Vec<ModelInfo>,
        err_list_models: bool,
    }

    impl FakeLlmService {
        fn claiming(id: &str) -> Self {
            Self {
                claims: vec![id.to_string()],
                ..Default::default()
            }
        }

        fn returning_text(id: &str, s: &str) -> Self {
            Self {
                claims: vec![id.to_string()],
                text: Some(s.to_string()),
                ..Default::default()
            }
        }

        fn with_models(models: Vec<ModelInfo>) -> Self {
            Self {
                models,
                ..Default::default()
            }
        }

        fn failing_list_models() -> Self {
            Self {
                err_list_models: true,
                ..Default::default()
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmService for FakeLlmService {
        async fn chat_stream(
            &self,
            _req: ChatRequest,
            _cancel: CancellationToken,
        ) -> BoxStream<'static, Result<ChatChunk, LlmError>> {
            let text = self.text.clone();
            Box::pin(futures::stream::iter(text.map_or_else(Vec::new, |t| {
                vec![Ok(ChatChunk {
                    delta: ChunkDelta::Text(t),
                    finish_reason: None,
                    usage: None,
                })]
            })))
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
            if self.err_list_models {
                Err(LlmError::BackendError("offline".into()))
            } else {
                Ok(self.models.clone())
            }
        }

        async fn status(
            &self,
            _backend_id: &str,
            _model_id: &str,
        ) -> Result<ModelStatus, LlmError> {
            Ok(ModelStatus {
                state: ModelState::Ready,
                progress: None,
            })
        }

        fn claims_backend(&self, backend_id: &str) -> bool {
            self.claims.iter().any(|c| c == backend_id)
        }
    }

    fn user_message(text: &str) -> ChatMessage {
        ChatMessage {
            role: ChatRole::User,
            content: ChatContent::Text(text.into()),
            tool_call_id: None,
            tool_calls: vec![],
        }
    }

    fn request(backend_id: &str) -> ChatRequest {
        ChatRequest {
            backend_id: backend_id.into(),
            model: "m".into(),
            messages: vec![user_message("hi")],
            params: Default::default(),
            tools: vec![],
            extra: serde_json::Value::Null,
        }
    }

    fn model(backend_id: &str, model_id: &str) -> ModelInfo {
        ModelInfo {
            backend_id: backend_id.into(),
            model_id: model_id.into(),
            display_name: model_id.into(),
            capabilities: Default::default(),
        }
    }

    #[tokio::test]
    async fn router_dispatches_by_backend_id() {
        let mut router = MultiBackendLlmService::new();
        router.register("a", Arc::new(FakeLlmService::returning_text("a", "from-a")));
        router.register("b", Arc::new(FakeLlmService::returning_text("b", "from-b")));

        let stream = router
            .chat_stream(request("b"), CancellationToken::new())
            .await;
        let chunks: Vec<_> = stream.collect().await;
        let got: Vec<_> = chunks
            .iter()
            .filter_map(|r| {
                r.as_ref().ok().and_then(|c| match &c.delta {
                    ChunkDelta::Text(s) => Some(s.as_str()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(got, vec!["from-b"]);
    }

    #[tokio::test]
    async fn router_unknown_backend_yields_invalid_request() {
        let router = MultiBackendLlmService::new();
        let stream = router
            .chat_stream(request("nope"), CancellationToken::new())
            .await;
        let first = stream.collect::<Vec<_>>().await;
        assert!(matches!(
            first.as_slice(),
            [Err(LlmError::InvalidRequest(_))]
        ));
    }

    #[tokio::test]
    async fn router_list_models_aggregates_across_backends() {
        let mut router = MultiBackendLlmService::new();
        router.register(
            "a",
            Arc::new(FakeLlmService::with_models(vec![model("a", "m1")])),
        );
        router.register(
            "b",
            Arc::new(FakeLlmService::with_models(vec![model("b", "m2")])),
        );

        let models = router.list_models().await.unwrap();
        assert_eq!(models.len(), 2);
        assert!(models
            .iter()
            .any(|m| m.backend_id == "a" && m.model_id == "m1"));
        assert!(models
            .iter()
            .any(|m| m.backend_id == "b" && m.model_id == "m2"));
    }

    #[tokio::test]
    async fn router_list_models_tolerates_per_backend_failure() {
        let mut router = MultiBackendLlmService::new();
        router.register("failing", Arc::new(FakeLlmService::failing_list_models()));
        router.register(
            "ok",
            Arc::new(FakeLlmService::with_models(vec![model("ok", "m1")])),
        );
        let models = router.list_models().await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_id, "m1");
    }

    #[tokio::test]
    async fn router_claims_backend_reflects_registrations() {
        let mut router = MultiBackendLlmService::new();
        router.register("a", Arc::new(FakeLlmService::claiming("alpha")));
        assert!(router.claims_backend("alpha"));
        assert!(!router.claims_backend("beta"));
    }

    #[tokio::test]
    async fn router_status_dispatches_to_claiming_backend() {
        let mut router = MultiBackendLlmService::new();
        router.register("a", Arc::new(FakeLlmService::claiming("alpha")));
        let status = router.status("alpha", "m").await.unwrap();
        assert_eq!(status.state, ModelState::Ready);

        assert!(matches!(
            router.status("unknown", "m").await,
            Err(LlmError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn router_load_model_unknown_backend_yields_error() {
        let router = MultiBackendLlmService::new();
        let mut stream = router.load_model("nope", "m", CancellationToken::new());
        let first = stream.next().await.unwrap();
        assert!(matches!(first, Err(LlmError::InvalidRequest(_))));
    }
}
