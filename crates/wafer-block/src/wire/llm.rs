//! Wire-format types for the LLM service.
//!
//! Mirrors `crates/wafer-core/src/interfaces/llm/handler.rs` and
//! `crates/wafer-core/src/interfaces/llm/service.rs`. The buffered request
//! and response shapes are defined here. Streaming chat ops emit a sequence
//! of `ChatChunk` values — each chunk is encoded as its own frame by Task 9
//! (`SDK ResponseStream`); the chunk shape itself is reused as-is.
//!
//! This module **redefines** the streaming-content types (`ChatRequest`,
//! `ChatChunk`, `ModelInfo`, etc.) here rather than re-exporting from
//! `wafer-core` — `wafer-block` is a leaf crate and cannot depend on
//! `wafer-core`. Field names + types match the existing `interfaces::llm::service`
//! types exactly so consumers can convert between them with `serde_json`
//! during migration. After Task 14 (handler migration), the wafer-core types
//! become thin re-exports of these.

use serde::{Deserialize, Serialize};

// ---- Chat request ----

/// Request for `llm.chat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Identifier of the configured LLM backend to dispatch to.
    pub backend_id: String,
    /// Model name within the backend.
    pub model: String,
    /// Conversation messages, in order.
    pub messages: Vec<ChatMessage>,
    /// Sampling / decoding parameters.
    #[serde(default)]
    pub params: ChatParams,
    /// Tools the model may call.
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    /// Backend-specific pass-through parameters (free-form JSON).
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// Sampling / decoding parameters for a chat request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatParams {
    /// Maximum number of output tokens to generate.
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Nucleus sampling top-p.
    pub top_p: Option<f32>,
    /// Strings that abort generation when emitted.
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    /// PRNG seed for deterministic sampling (if supported).
    pub seed: Option<u64>,
    /// Constrain output to a textual / JSON / schema-shaped format.
    pub response_format: Option<ResponseFormat>,
}

/// Output-format constraint requested from the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseFormat {
    /// Unconstrained text output.
    Text,
    /// Free-form JSON output.
    Json,
    /// JSON output matching the given JSON Schema.
    JsonSchema(serde_json::Value),
}

/// One message in a chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Role of the message author.
    pub role: ChatRole,
    /// Message content (plain text or multipart).
    pub content: ChatContent,
    /// When `role == Tool`, the id of the tool call this message responds to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// When `role == Assistant`, tool calls the model issued in this turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

/// Author role of a [`ChatMessage`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChatRole {
    /// System / instructions message.
    System,
    /// End-user message.
    User,
    /// Model response message.
    Assistant,
    /// Tool result message.
    Tool,
}

/// Body of a chat message — either a plain string or a multipart sequence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChatContent {
    /// Plain text content.
    Text(String),
    /// Multipart content (mixed text + images).
    Parts(Vec<ContentPart>),
}

/// One element of a multipart [`ChatContent::Parts`] sequence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContentPart {
    /// Plain-text fragment.
    Text(String),
    /// Image referenced by URL (with optional resolution hint).
    ImageUrl {
        /// Image URL.
        url: String,
        /// Optional `"low"`/`"high"`/`"auto"` detail hint.
        detail: Option<String>,
    },
    /// Inline image bytes (`mime_type` describes encoding).
    ImageBytes {
        /// Raw image bytes.
        bytes: Vec<u8>,
        /// MIME type of `bytes`.
        mime_type: String,
    },
}

/// Tool descriptor passed to the model (JSON-Schema parameters).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name.
    pub name: String,
    /// Natural-language description shown to the model.
    pub description: String,
    /// JSON Schema describing the tool's input arguments.
    pub parameters: serde_json::Value,
}

/// A tool invocation issued by the model in a single turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Per-call id used to correlate the result message back to this call.
    pub id: String,
    /// Tool name (must match a [`ToolDefinition::name`]).
    pub name: String,
    /// JSON arguments the model produced for this call.
    pub arguments: serde_json::Value,
}

// ---- Chat streaming chunk ----

/// One frame in a streaming chat response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChunk {
    /// Incremental update for this frame.
    pub delta: ChunkDelta,
    /// When set, marks the terminal frame and explains why generation stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    /// Final token-usage statistics (emitted on or near the terminal frame).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

/// Incremental payload variant carried by [`ChatChunk::delta`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChunkDelta {
    /// Appended assistant text.
    Text(String),
    /// A new tool call has begun.
    ToolCallStart {
        /// Tool-call id.
        id: String,
        /// Tool name.
        name: String,
    },
    /// Incremental JSON-arguments fragment for an in-progress tool call.
    ToolCallArguments {
        /// Tool-call id.
        id: String,
        /// Newly-emitted arguments fragment to append.
        arguments_delta: String,
    },
    /// All arguments for the named tool call have been delivered.
    ToolCallComplete {
        /// Tool-call id.
        id: String,
    },
    /// Carries no incremental payload (used for terminal usage/finish frames).
    Empty,
}

/// Why generation stopped, as reported in the terminal [`ChatChunk`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FinishReason {
    /// Natural stop / EOS reached.
    Stop,
    /// Output token limit reached.
    Length,
    /// Generation paused for tool-call resolution.
    ToolCall,
    /// Output was blocked by a content filter.
    ContentFilter,
    /// Backend reported an error.
    Error,
}

/// Token-usage statistics for a completed chat request.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    /// Number of input/prompt tokens.
    pub input_tokens: u32,
    /// Number of output/completion tokens.
    pub output_tokens: u32,
    /// Cached prompt tokens (backend-specific).
    pub cached_tokens: Option<u32>,
    /// Reasoning tokens (e.g. internal chain-of-thought).
    pub reasoning_tokens: Option<u32>,
}

// ---- Status / model management ----

/// Request for `llm.status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusRequest {
    /// Backend identifier.
    pub backend_id: String,
    /// Model identifier within the backend.
    pub model_id: String,
}

/// Request for `llm.load_model`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadModelRequest {
    /// Backend identifier.
    pub backend_id: String,
    /// Model identifier to load.
    pub model_id: String,
}

/// Request for `llm.unload_model`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnloadModelRequest {
    /// Backend identifier.
    pub backend_id: String,
    /// Model identifier to unload.
    pub model_id: String,
}

/// Descriptor of an available model returned by `llm.list_models`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Backend identifier the model is hosted in.
    pub backend_id: String,
    /// Backend-side model identifier.
    pub model_id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Declared model capabilities.
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

/// Capability flags for a [`ModelInfo`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Supports streaming responses.
    pub streaming: bool,
    /// Supports tool / function calling.
    pub tools: bool,
    /// Accepts image inputs.
    pub vision: bool,
    /// Supports structured JSON output mode.
    pub json_mode: bool,
    /// Maximum input context window in tokens.
    pub max_context_tokens: Option<u32>,
    /// Maximum output tokens per request.
    pub max_output_tokens: Option<u32>,
}

/// Loaded-model status returned by `llm.status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatus {
    /// Current model state.
    pub state: ModelState,
    /// Optional 0.0..1.0 progress when `state == Loading`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
}

/// Lifecycle state of a model on a backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelState {
    /// Model is loaded and ready to serve requests.
    Ready,
    /// Model is currently being loaded.
    Loading,
    /// Model is known to the backend but not loaded.
    Unloaded,
    /// Loading or serving failed.
    Error {
        /// Error description.
        message: String,
    },
}

/// Progress detail for a model-load operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadProgress {
    /// Free-form stage label (e.g. `"downloading"`, `"verifying"`).
    pub stage: String,
    /// Bytes downloaded so far (if known).
    pub bytes_downloaded: Option<u64>,
    /// Expected total bytes (if known).
    pub bytes_total: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    #[test]
    fn chat_request_round_trips() {
        let original = ChatRequest {
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
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ChatRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.backend_id, "openai-main");
        assert_eq!(decoded.messages.len(), 1);
        assert_eq!(decoded.params.temperature, Some(0.7));
    }

    #[test]
    fn chat_chunk_text_round_trips() {
        let original = ChatChunk {
            delta: ChunkDelta::Text("hello".into()),
            finish_reason: None,
            usage: None,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ChatChunk = codec::decode(&encoded).expect("decode");
        assert!(matches!(decoded.delta, ChunkDelta::Text(ref s) if s == "hello"));
    }

    #[test]
    fn chat_chunk_terminal_with_usage_round_trips() {
        let original = ChatChunk {
            delta: ChunkDelta::Empty,
            finish_reason: Some(FinishReason::Stop),
            usage: Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                cached_tokens: Some(4),
                reasoning_tokens: None,
            }),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ChatChunk = codec::decode(&encoded).expect("decode");
        assert!(matches!(decoded.finish_reason, Some(FinishReason::Stop)));
        let usage = decoded.usage.unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 20);
    }

    #[test]
    fn status_request_round_trips() {
        let original = StatusRequest {
            backend_id: "b1".into(),
            model_id: "m1".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: StatusRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.backend_id, "b1");
        assert_eq!(decoded.model_id, "m1");
    }

    #[test]
    fn model_info_round_trips() {
        let original = ModelInfo {
            backend_id: "b".into(),
            model_id: "m".into(),
            display_name: "M".into(),
            capabilities: ModelCapabilities {
                streaming: true,
                tools: true,
                ..Default::default()
            },
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ModelInfo = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.backend_id, "b");
        assert!(decoded.capabilities.streaming);
        assert!(decoded.capabilities.tools);
    }

    // ----- Schema locks -----

    #[test]
    fn schema_lock_status_request() {
        let req = StatusRequest {
            backend_id: String::new(),
            model_id: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82aa6261636b656e645f6964a0a86d6f64656c5f6964a0",
            "StatusRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_load_model_request() {
        let req = LoadModelRequest {
            backend_id: String::new(),
            model_id: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82aa6261636b656e645f6964a0a86d6f64656c5f6964a0",
            "LoadModelRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_unload_model_request() {
        let req = UnloadModelRequest {
            backend_id: String::new(),
            model_id: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82aa6261636b656e645f6964a0a86d6f64656c5f6964a0",
            "UnloadModelRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_token_usage() {
        let u = TokenUsage::default();
        let encoded = codec::encode(&u).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "84ac696e7075745f746f6b656e7300ad6f75747075745f746f6b656e7300ad6361636865645f746f6b656e73c0b0726561736f6e696e675f746f6b656e73c0",
            "TokenUsage schema changed — review consumer impact before updating this literal"
        );
    }
}
