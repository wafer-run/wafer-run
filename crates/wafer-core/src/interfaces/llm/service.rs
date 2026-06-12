//! LlmService trait + shared data types.
//!
//! Mirrors the layout of `interfaces::database::service`. See solobase spec
//! `2026-04-15-llm-service-refactor-design.md`.
//!
//! The data types (`ChatRequest`, `ChatChunk`, `ModelInfo`, …) are the
//! canonical wire types from `wafer_block::wire::llm`, re-exported here so
//! service impls and wire-level consumers share one definition — there is no
//! separate service-side representation and no conversion layer. Only the
//! genuinely service-side items (`LlmService`, `LlmError`) live in this
//! module.

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use wafer_block_macro::wafer_async_trait;

pub use wafer_block::wire::llm::{
    ChatChunk, ChatContent, ChatMessage, ChatParams, ChatRequest, ChatRole, ChunkDelta,
    ContentPart, FinishReason, LoadProgress, ModelCapabilities, ModelInfo, ModelState, ModelStatus,
    ResponseFormat, TokenUsage, ToolCall, ToolDefinition,
};

// ---------- Error ----------

/// Errors returned by [`LlmService`] operations.
#[derive(Debug, Error, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LlmError {
    /// Operation is not implemented by this backend.
    #[error("not supported by this backend")]
    NotSupported,
    /// Caller supplied an invalid request.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// Backend reported an internal failure.
    #[error("backend error: {0}")]
    BackendError(String),
    /// Requested model is not known to the backend.
    #[error("model not found: {0}")]
    ModelNotFound(String),
    /// Backend rejected the call due to rate limiting.
    #[error("rate limited")]
    RateLimited,
    /// Backend rejected the call due to missing / invalid credentials.
    #[error("unauthorized")]
    Unauthorized,
    /// Network-level failure (e.g. upstream provider).
    #[error("network error: {0}")]
    Network(String),
    /// Operation was cancelled via its `CancellationToken`.
    #[error("cancelled")]
    Cancelled,
}

// ---------- Trait ----------

/// Backend-agnostic LLM service. Implementations expose chat streaming plus
/// model enumeration / load-management for backends that support it.
///
/// The trait is streaming-native: `chat_stream` returns a stream of
/// `ChatChunk`s rather than a single buffered response. Consumers can surface
/// the stream end-to-end (SSE over HTTP, raw `ReadableStream` in the browser,
/// direct iteration in-process). `load_model` is also stream-returning for
/// the same reason: weight downloads and initialization are long-running and
/// users expect progress reporting.
///
/// `MaybeSend + MaybeSync` follows the same pattern as `DatabaseService`:
/// `Send + Sync` on native, unbounded on `wasm32` (where futures aren't
/// required to be `Send`).
#[wafer_async_trait]
pub trait LlmService: wafer_block::MaybeSend + wafer_block::MaybeSync + 'static {
    /// Stream of chat chunks for the given request.
    ///
    /// The returned stream may yield `Err` mid-stream; callers decide how to
    /// surface partial output. The stream ends naturally when generation
    /// completes. `cancel` is checked during long awaits by the impl and fires
    /// when the consumer drops the stream.
    async fn chat_stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ChatChunk, LlmError>>;

    /// All models this service exposes across its backends. The router
    /// aggregates this across every registered impl.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError>;

    /// Current status for a `(backend, model)` pair. Remote backends typically
    /// return `ModelState::Ready` (or `Error` if unreachable); local backends
    /// distinguish `Unloaded` / `Loading` / `Ready`.
    async fn status(&self, backend_id: &str, model_id: &str) -> Result<ModelStatus, LlmError>;

    /// Stream of load-progress events, terminating with the final status or an
    /// error. Default impl emits a single `NotSupported` error — backends that
    /// only serve remote APIs don't need to override.
    fn load_model(
        &self,
        _backend_id: &str,
        _model_id: &str,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<LoadProgress, LlmError>> {
        Box::pin(futures::stream::once(async { Err(LlmError::NotSupported) }))
    }

    /// Unload a locally-loaded model. Default impl returns `NotSupported`.
    async fn unload_model(&self, _backend_id: &str, _model_id: &str) -> Result<(), LlmError> {
        Err(LlmError::NotSupported)
    }

    /// Whether this impl handles the given `backend_id`. Called by the router
    /// to dispatch requests; must be cheap — typically a hash lookup or prefix
    /// match. Default returns `false`; every concrete impl must override.
    fn claims_backend(&self, _backend_id: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_error_display_and_roundtrip() {
        let err = LlmError::InvalidRequest("missing model".into());
        assert_eq!(err.to_string(), "invalid request: missing model");
        let json = serde_json::to_string(&err).unwrap();
        let decoded: LlmError = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, err);
    }

    /// Minimal impl that only overrides the required trait methods. Exists to
    /// exercise the default impls for `load_model`, `unload_model`, and
    /// `claims_backend`.
    struct MinimalLlm;

    #[async_trait::async_trait]
    impl LlmService for MinimalLlm {
        async fn chat_stream(
            &self,
            _req: ChatRequest,
            _cancel: CancellationToken,
        ) -> BoxStream<'static, Result<ChatChunk, LlmError>> {
            Box::pin(futures::stream::empty())
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
            Ok(vec![])
        }

        async fn status(
            &self,
            _backend_id: &str,
            _model_id: &str,
        ) -> Result<ModelStatus, LlmError> {
            Ok(ModelStatus::ready())
        }
    }

    #[tokio::test]
    async fn default_load_model_returns_not_supported() {
        use futures::StreamExt;
        let svc = MinimalLlm;
        let mut stream = svc.load_model("x", "y", CancellationToken::new());
        let first = stream.next().await.unwrap();
        assert!(matches!(first, Err(LlmError::NotSupported)));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn default_unload_model_returns_not_supported() {
        let svc = MinimalLlm;
        assert!(matches!(
            svc.unload_model("x", "y").await,
            Err(LlmError::NotSupported)
        ));
    }

    #[test]
    fn default_claims_backend_returns_false() {
        let svc = MinimalLlm;
        assert!(!svc.claims_backend("anything"));
    }
}
