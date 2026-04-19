//! LlmService trait + shared data types.
//!
//! Mirrors the layout of `interfaces::database::service`. See solobase spec
//! `2026-04-15-llm-service-refactor-design.md`.

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

// ---------- Request side ----------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ChatRequest {
    /// Router key. Matches a specific provider registered on the underlying
    /// `LlmService` impl (e.g. `"openai-main"`, `"local-llama"`, `"webllm-smollm2"`).
    pub backend_id: String,
    /// Model id within the backend.
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub params: ChatParams,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    /// Backend-specific parameter overflow.
    #[serde(default)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ChatParams {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    pub seed: Option<u64>,
    pub response_format: Option<ResponseFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub enum ResponseFormat {
    Text,
    Json,
    JsonSchema(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: ChatContent,
    /// Set on `Role::Tool` messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Set on `Role::Assistant` messages that invoke tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub enum ChatContent {
    Text(String),
    /// Multimodal content. Impls that don't support the requested parts should
    /// return `LlmError::NotSupported`.
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub enum ContentPart {
    Text(String),
    ImageUrl { url: String, detail: Option<String> },
    ImageBytes { bytes: Vec<u8>, mime_type: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the tool's arguments.
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl ChatRequest {
    /// Minimal constructor. Leaves `params`, `tools`, and `extra` defaulted;
    /// callers set them via field access.
    pub fn new(
        backend_id: impl Into<String>,
        model: impl Into<String>,
        messages: Vec<ChatMessage>,
    ) -> Self {
        Self {
            backend_id: backend_id.into(),
            model: model.into(),
            messages,
            params: ChatParams::default(),
            tools: Vec::new(),
            extra: serde_json::Value::Null,
        }
    }
}

impl ChatMessage {
    /// Text-only user message.
    pub fn user(text: impl Into<String>) -> Self {
        Self::text(ChatRole::User, text)
    }

    /// Text-only system message.
    pub fn system(text: impl Into<String>) -> Self {
        Self::text(ChatRole::System, text)
    }

    /// Text-only assistant message.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text(ChatRole::Assistant, text)
    }

    /// Tool-result message.
    pub fn tool(tool_call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Tool,
            content: ChatContent::Text(text.into()),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: Vec::new(),
        }
    }

    fn text(role: ChatRole, text: impl Into<String>) -> Self {
        Self {
            role,
            content: ChatContent::Text(text.into()),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }
}

// ---------- Response side ----------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ChatChunk {
    pub delta: ChunkDelta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    /// Present on the terminal chunk when the backend reports usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub enum ChunkDelta {
    Text(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallArguments {
        id: String,
        arguments_delta: String,
    },
    ToolCallComplete {
        id: String,
    },
    /// Meta-only chunk (heartbeats, usage updates).
    Empty,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum FinishReason {
    Stop,
    Length,
    ToolCall,
    ContentFilter,
    Error,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: Option<u32>,
    pub reasoning_tokens: Option<u32>,
}

impl ChatChunk {
    /// A non-terminal text delta.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            delta: ChunkDelta::Text(s.into()),
            finish_reason: None,
            usage: None,
        }
    }

    /// A terminal chunk with the given finish reason and optional usage. The
    /// delta is `Empty` — a meta-only terminal frame.
    pub fn finish(reason: FinishReason, usage: Option<TokenUsage>) -> Self {
        Self {
            delta: ChunkDelta::Empty,
            finish_reason: Some(reason),
            usage,
        }
    }
}

// ---------- Model management ----------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ModelInfo {
    pub backend_id: String,
    pub model_id: String,
    pub display_name: String,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelCapabilities {
    pub streaming: bool,
    pub tools: bool,
    pub vision: bool,
    pub json_mode: bool,
    pub max_context_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ModelStatus {
    pub state: ModelState,
    /// 0.0–1.0 when `state == Loading`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelState {
    /// Local: weights loaded. Remote: endpoint reachable.
    Ready,
    Loading,
    /// Local only — weights not in memory.
    Unloaded,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoadProgress {
    /// e.g. `"downloading"`, `"initializing"`, `"compiling"`.
    pub stage: String,
    pub bytes_downloaded: Option<u64>,
    pub bytes_total: Option<u64>,
}

impl ModelInfo {
    /// Minimal constructor. Capabilities default to all-false / unlimited.
    pub fn new(
        backend_id: impl Into<String>,
        model_id: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Self {
        Self {
            backend_id: backend_id.into(),
            model_id: model_id.into(),
            display_name: display_name.into(),
            capabilities: ModelCapabilities::default(),
        }
    }

    pub fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

impl LoadProgress {
    /// Minimal constructor. Byte counters default to `None`; callers set them
    /// via field access when the backend reports them.
    pub fn new(stage: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            bytes_downloaded: None,
            bytes_total: None,
        }
    }
}

impl ModelStatus {
    pub fn ready() -> Self {
        Self {
            state: ModelState::Ready,
            progress: None,
        }
    }

    pub fn loading(progress: f32) -> Self {
        Self {
            state: ModelState::Loading,
            progress: Some(progress),
        }
    }

    pub fn unloaded() -> Self {
        Self {
            state: ModelState::Unloaded,
            progress: None,
        }
    }
}

// ---------- Error ----------

#[derive(Debug, Error, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LlmError {
    #[error("not supported by this backend")]
    NotSupported,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("backend error: {0}")]
    BackendError(String),
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("rate limited")]
    RateLimited,
    #[error("unauthorized")]
    Unauthorized,
    #[error("network error: {0}")]
    Network(String),
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
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
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
    fn chat_request_roundtrip_text_message() {
        let req = ChatRequest {
            backend_id: "openai-main".into(),
            model: "gpt-4o-mini".into(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: ChatContent::Text("hi".into()),
                tool_call_id: None,
                tool_calls: vec![],
            }],
            params: ChatParams {
                temperature: Some(0.7),
                ..Default::default()
            },
            tools: vec![],
            extra: serde_json::Value::Null,
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: ChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn chat_chunk_text_delta_roundtrip() {
        let chunk = ChatChunk {
            delta: ChunkDelta::Text("hello".into()),
            finish_reason: None,
            usage: None,
        };
        let json = serde_json::to_string(&chunk).unwrap();
        assert!(json.contains("\"Text\":\"hello\""));
        let decoded: ChatChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, chunk);
    }

    #[test]
    fn chat_chunk_tool_call_arguments_roundtrip() {
        let chunk = ChatChunk {
            delta: ChunkDelta::ToolCallArguments {
                id: "call_1".into(),
                arguments_delta: "{\"x\":".into(),
            },
            finish_reason: None,
            usage: None,
        };
        let json = serde_json::to_string(&chunk).unwrap();
        let decoded: ChatChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, chunk);
    }

    #[test]
    fn chat_chunk_terminal_with_usage_roundtrip() {
        let chunk = ChatChunk {
            delta: ChunkDelta::Empty,
            finish_reason: Some(FinishReason::Stop),
            usage: Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                cached_tokens: Some(4),
                reasoning_tokens: None,
            }),
        };
        let json = serde_json::to_string(&chunk).unwrap();
        let decoded: ChatChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, chunk);
    }

    #[test]
    fn chat_request_with_tools_roundtrip() {
        let req = ChatRequest {
            backend_id: "openai-main".into(),
            model: "gpt-4o".into(),
            messages: vec![],
            params: ChatParams::default(),
            tools: vec![ToolDefinition {
                name: "lookup".into(),
                description: "look something up".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            extra: serde_json::Value::Null,
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: ChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn model_info_roundtrip() {
        let info = ModelInfo {
            backend_id: "openai-main".into(),
            model_id: "gpt-4o".into(),
            display_name: "GPT-4o".into(),
            capabilities: ModelCapabilities {
                streaming: true,
                tools: true,
                vision: true,
                max_context_tokens: Some(128_000),
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&info).unwrap();
        let decoded: ModelInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, info);
    }

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
            Ok(ModelStatus {
                state: ModelState::Ready,
                progress: None,
            })
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
