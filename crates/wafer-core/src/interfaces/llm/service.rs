//! LlmService trait + shared data types.
//!
//! Mirrors the layout of `interfaces::database::service`. See solobase spec
//! `2026-04-15-llm-service-refactor-design.md`.

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use wafer_block_macro::wafer_async_trait;

// ---------- Request side ----------

/// A chat-completion request directed at a specific backend / model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ChatRequest {
    /// Router key. Matches a specific provider registered on the underlying
    /// `LlmService` impl (e.g. `"openai-main"`, `"local-llama"`, `"webllm-smollm2"`).
    pub backend_id: String,
    /// Model id within the backend.
    pub model: String,
    /// Conversation history (system / user / assistant / tool messages).
    pub messages: Vec<ChatMessage>,
    /// Generation parameters (temperature, max tokens, …).
    #[serde(default)]
    pub params: ChatParams,
    /// Tool / function definitions the model may call.
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    /// Backend-specific parameter overflow.
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// Generation parameters shared across most chat backends.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ChatParams {
    /// Maximum tokens to generate.
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Nucleus-sampling top-p.
    pub top_p: Option<f32>,
    /// Stop sequences that terminate generation.
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    /// Deterministic sampling seed.
    pub seed: Option<u64>,
    /// Constrain the response format (text, JSON, JSON Schema).
    pub response_format: Option<ResponseFormat>,
}

/// Constraint on the model's response format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub enum ResponseFormat {
    /// Free-form text (default).
    Text,
    /// Any valid JSON value.
    Json,
    /// JSON conforming to the supplied JSON Schema.
    JsonSchema(serde_json::Value),
}

/// A single message in a [`ChatRequest`] conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ChatMessage {
    /// Who is speaking.
    pub role: ChatRole,
    /// Message content (text or multimodal).
    pub content: ChatContent,
    /// Set on `Role::Tool` messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Set on `Role::Assistant` messages that invoke tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

/// Speaker role attached to a [`ChatMessage`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChatRole {
    /// System / developer instruction.
    System,
    /// End-user input.
    User,
    /// Model output.
    Assistant,
    /// Tool / function result fed back to the model.
    Tool,
}

/// Content of a [`ChatMessage`] — plain text or a multimodal part list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub enum ChatContent {
    /// Single text chunk.
    Text(String),
    /// Multimodal content. Impls that don't support the requested parts should
    /// return `LlmError::NotSupported`.
    Parts(Vec<ContentPart>),
}

/// A single part of a multimodal [`ChatContent::Parts`] message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub enum ContentPart {
    /// Text fragment.
    Text(String),
    /// Image referenced by URL.
    ImageUrl {
        /// Image URL.
        url: String,
        /// Optional backend-specific detail hint (e.g. `"auto"`, `"high"`).
        detail: Option<String>,
    },
    /// Image supplied inline as bytes.
    ImageBytes {
        /// Raw image bytes.
        bytes: Vec<u8>,
        /// MIME type of `bytes` (e.g. `image/png`).
        mime_type: String,
    },
}

/// Definition of a tool / function the model may call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ToolDefinition {
    /// Tool name as advertised to the model.
    pub name: String,
    /// Human-readable description; the model uses this to decide when to call the tool.
    pub description: String,
    /// JSON Schema describing the tool's arguments.
    pub parameters: serde_json::Value,
}

/// A tool invocation emitted by the model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ToolCall {
    /// Unique id for correlating this call with its later `Role::Tool` response.
    pub id: String,
    /// Name of the tool being invoked.
    pub name: String,
    /// Arguments the model produced (interpreted per the tool's JSON Schema).
    pub arguments: serde_json::Value,
}

impl ToolDefinition {
    /// Build a `ToolDefinition` from its `name`, `description`, and parameter schema.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

impl ToolCall {
    /// Build a `ToolCall` from its `id`, tool `name`, and `arguments`.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
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

    /// Construct a message with arbitrary role + content (no tool fields set).
    pub fn new(role: ChatRole, content: ChatContent) -> Self {
        Self {
            role,
            content,
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }

    /// Builder: attach tool-call results to an assistant message.
    pub fn with_tool_calls(mut self, tool_calls: Vec<ToolCall>) -> Self {
        self.tool_calls = tool_calls;
        self
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

/// A single streamed event from [`LlmService::chat_stream`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ChatChunk {
    /// Incremental update carried by this chunk.
    pub delta: ChunkDelta,
    /// Reason generation stopped, set on the terminal chunk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    /// Present on the terminal chunk when the backend reports usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

/// Incremental delta carried by a [`ChatChunk`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub enum ChunkDelta {
    /// Additional text appended to the assistant response.
    Text(String),
    /// A tool call is starting; subsequent `ToolCallArguments` deltas append to it.
    ToolCallStart {
        /// Tool-call id for correlation.
        id: String,
        /// Tool name being invoked.
        name: String,
    },
    /// Incremental piece of a tool call's `arguments` JSON.
    ToolCallArguments {
        /// Tool-call id this delta belongs to.
        id: String,
        /// Partial arguments JSON fragment.
        arguments_delta: String,
    },
    /// The tool call with the given id has been fully transmitted.
    ToolCallComplete {
        /// Tool-call id that completed.
        id: String,
    },
    /// Meta-only chunk (heartbeats, usage updates).
    Empty,
}

/// Reason an LLM stream terminated.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum FinishReason {
    /// Model emitted a natural stop / EOS token.
    Stop,
    /// Generation hit the `max_tokens` budget.
    Length,
    /// Model requested a tool call; the runtime should execute it and resume.
    ToolCall,
    /// Output was filtered by the backend's content-safety system.
    ContentFilter,
    /// Generation aborted with an error.
    Error,
}

/// Token-usage accounting reported by the backend.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct TokenUsage {
    /// Tokens billed for the prompt / input.
    pub input_tokens: u32,
    /// Tokens billed for the generated output.
    pub output_tokens: u32,
    /// Tokens served from a prompt cache, if the backend exposes the metric.
    pub cached_tokens: Option<u32>,
    /// Tokens spent on hidden reasoning, if the backend exposes the metric.
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

    /// A non-terminal chunk carrying any delta variant (for tool-call events).
    pub fn delta(delta: ChunkDelta) -> Self {
        Self {
            delta,
            finish_reason: None,
            usage: None,
        }
    }

    /// A chunk announcing the start of a tool call.
    pub fn tool_call_start(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            delta: ChunkDelta::ToolCallStart {
                id: id.into(),
                name: name.into(),
            },
            finish_reason: None,
            usage: None,
        }
    }

    /// A chunk carrying an incremental tool-call arguments delta.
    pub fn tool_call_arguments(id: impl Into<String>, arguments_delta: impl Into<String>) -> Self {
        Self {
            delta: ChunkDelta::ToolCallArguments {
                id: id.into(),
                arguments_delta: arguments_delta.into(),
            },
            finish_reason: None,
            usage: None,
        }
    }

    /// A chunk announcing the end of a tool call.
    pub fn tool_call_complete(id: impl Into<String>) -> Self {
        Self {
            delta: ChunkDelta::ToolCallComplete { id: id.into() },
            finish_reason: None,
            usage: None,
        }
    }

    /// A meta-only chunk carrying just a usage update.
    pub fn usage(usage: TokenUsage) -> Self {
        Self {
            delta: ChunkDelta::Empty,
            finish_reason: None,
            usage: Some(usage),
        }
    }
}

impl TokenUsage {
    /// Construct with input + output token counts. cached/reasoning default to None.
    pub fn new(input_tokens: u32, output_tokens: u32) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cached_tokens: None,
            reasoning_tokens: None,
        }
    }

    /// Set the cached-tokens field.
    pub fn with_cached(mut self, cached: u32) -> Self {
        self.cached_tokens = Some(cached);
        self
    }

    /// Set the reasoning-tokens field.
    pub fn with_reasoning(mut self, reasoning: u32) -> Self {
        self.reasoning_tokens = Some(reasoning);
        self
    }
}

// ---------- Model management ----------

/// Metadata describing a model exposed by an LLM backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ModelInfo {
    /// Backend that owns this model.
    pub backend_id: String,
    /// Model id within the backend.
    pub model_id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Optional capability hints.
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

/// Capability hints describing what a model supports.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelCapabilities {
    /// Supports streaming chat output.
    pub streaming: bool,
    /// Supports tool / function calling.
    pub tools: bool,
    /// Supports image / multimodal input.
    pub vision: bool,
    /// Supports structured JSON output mode.
    pub json_mode: bool,
    /// Maximum input context size in tokens, if known.
    pub max_context_tokens: Option<u32>,
    /// Maximum generated output size in tokens, if known.
    pub max_output_tokens: Option<u32>,
}

/// Current lifecycle status of a model on a backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ModelStatus {
    /// High-level state.
    pub state: ModelState,
    /// 0.0–1.0 when `state == Loading`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
}

/// High-level lifecycle state of a backend model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelState {
    /// Local: weights loaded. Remote: endpoint reachable.
    Ready,
    /// Local: weights currently downloading or initializing.
    Loading,
    /// Local only — weights not in memory.
    Unloaded,
    /// Model is in a failed state.
    Error {
        /// Failure message.
        message: String,
    },
}

/// Streaming progress event emitted by [`LlmService::load_model`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoadProgress {
    /// e.g. `"downloading"`, `"initializing"`, `"compiling"`.
    pub stage: String,
    /// Bytes downloaded so far, if known.
    pub bytes_downloaded: Option<u64>,
    /// Total bytes to download, if known.
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

    /// Replace the `capabilities` field; chainable on `new()`.
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
    /// A status reporting the model is ready to serve.
    pub fn ready() -> Self {
        Self {
            state: ModelState::Ready,
            progress: None,
        }
    }

    /// A status reporting the model is currently loading at the given progress fraction.
    pub fn loading(progress: f32) -> Self {
        Self {
            state: ModelState::Loading,
            progress: Some(progress),
        }
    }

    /// A status reporting the model is known but not currently resident.
    pub fn unloaded() -> Self {
        Self {
            state: ModelState::Unloaded,
            progress: None,
        }
    }

    /// An errored status carrying the failure message. Progress is cleared.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            state: ModelState::Error {
                message: message.into(),
            },
            progress: None,
        }
    }
}

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

    #[test]
    fn chat_chunk_tool_call_constructors_build_expected_variants() {
        let start = ChatChunk::tool_call_start("call_1", "search");
        assert!(matches!(
            start.delta,
            ChunkDelta::ToolCallStart { ref id, ref name } if id == "call_1" && name == "search"
        ));
        assert!(start.finish_reason.is_none());

        let args = ChatChunk::tool_call_arguments("call_1", "{\"q\":");
        assert!(matches!(
            args.delta,
            ChunkDelta::ToolCallArguments { ref id, ref arguments_delta }
                if id == "call_1" && arguments_delta == "{\"q\":"
        ));

        let done = ChatChunk::tool_call_complete("call_1");
        assert!(matches!(done.delta, ChunkDelta::ToolCallComplete { ref id } if id == "call_1"));

        let usage = ChatChunk::usage(TokenUsage::new(10, 20));
        assert_eq!(usage.delta, ChunkDelta::Empty);
        assert_eq!(usage.usage.as_ref().map(|u| u.input_tokens), Some(10));
        assert_eq!(usage.usage.as_ref().map(|u| u.output_tokens), Some(20));
    }

    #[test]
    fn token_usage_new_builder_sets_required_fields_and_defaults() {
        let u = TokenUsage::new(7, 11);
        assert_eq!(u.input_tokens, 7);
        assert_eq!(u.output_tokens, 11);
        assert!(u.cached_tokens.is_none());
        assert!(u.reasoning_tokens.is_none());

        let u2 = TokenUsage::new(1, 2).with_cached(3).with_reasoning(4);
        assert_eq!(u2.cached_tokens, Some(3));
        assert_eq!(u2.reasoning_tokens, Some(4));
    }
}
