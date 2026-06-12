//! Wire-format types for the LLM service.
//!
//! These are the canonical chat / model-management types: the buffered
//! request and response shapes are defined here, and
//! `wafer_core::interfaces::llm::service` re-exports them so host-side
//! service impls and wire-level consumers share one definition. Streaming
//! chat ops emit a sequence of `ChatChunk` values — each chunk is encoded as
//! its own frame; the chunk shape itself is reused as-is.
//!
//! The model-management types (`StatusRequest`, `LoadModelRequest`,
//! `UnloadModelRequest`, `ModelStatus`, `ModelState`, `LoadProgress`) are
//! shared verbatim with the image service and live in
//! [`super::model_common`]; they are re-exported here so existing
//! `wire::llm::*` import paths keep working.

use serde::{Deserialize, Serialize};

pub use super::model_common::{
    LoadModelRequest, LoadProgress, ModelState, ModelStatus, StatusRequest, UnloadModelRequest,
};

// ---- Chat request ----

/// Request for `llm.chat`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

/// Sampling / decoding parameters for a chat request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ResponseFormat {
    /// Unconstrained text output.
    Text,
    /// Free-form JSON output.
    Json,
    /// JSON output matching the given JSON Schema.
    JsonSchema(serde_json::Value),
}

/// One message in a chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// Multipart content (mixed text + images). Impls that don't support the
    /// requested parts should return `LlmError::NotSupported`.
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    /// Tool name.
    pub name: String,
    /// Natural-language description shown to the model.
    pub description: String,
    /// JSON Schema describing the tool's input arguments.
    pub parameters: serde_json::Value,
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

/// A tool invocation issued by the model in a single turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    /// Per-call id used to correlate the result message back to this call.
    pub id: String,
    /// Tool name (must match a [`ToolDefinition::name`]).
    pub name: String,
    /// JSON arguments the model produced for this call.
    pub arguments: serde_json::Value,
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

// ---- Chat streaming chunk ----

/// One frame in a streaming chat response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

/// Incremental payload variant carried by [`ChatChunk::delta`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

// ---- Model management ----

/// Descriptor of an available model returned by `llm.list_models`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

/// Capability flags for a [`ModelInfo`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    /// Representative request: params, tools, multipart content, and an
    /// assistant turn carrying tool calls. Full decode equality pins the wire
    /// shape end-to-end.
    #[test]
    fn chat_request_round_trips() {
        let original = ChatRequest {
            backend_id: "openai-main".into(),
            model: "gpt-4o-mini".into(),
            messages: vec![
                ChatMessage::system("be terse"),
                ChatMessage {
                    role: ChatRole::User,
                    content: ChatContent::Parts(vec![
                        ContentPart::Text("what is this?".into()),
                        ContentPart::ImageUrl {
                            url: "https://example.com/cat.png".into(),
                            detail: Some("high".into()),
                        },
                    ]),
                    tool_call_id: None,
                    tool_calls: vec![],
                },
                ChatMessage::assistant("checking").with_tool_calls(vec![ToolCall::new(
                    "call_1",
                    "lookup",
                    serde_json::json!({"q": "cat"}),
                )]),
                ChatMessage::tool("call_1", "a cat"),
            ],
            params: ChatParams {
                max_tokens: Some(256),
                temperature: Some(0.7),
                top_p: Some(0.9),
                stop_sequences: vec!["END".into()],
                seed: Some(42),
                response_format: Some(ResponseFormat::JsonSchema(
                    serde_json::json!({"type": "object"}),
                )),
            },
            tools: vec![ToolDefinition::new(
                "lookup",
                "look something up",
                serde_json::json!({"type": "object"}),
            )],
            extra: serde_json::json!({"provider_knob": true}),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ChatRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn chat_chunk_text_round_trips() {
        let original = ChatChunk::text("hello");
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ChatChunk = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn chat_chunk_terminal_with_usage_round_trips() {
        let original = ChatChunk::finish(
            FinishReason::Stop,
            Some(TokenUsage::new(10, 20).with_cached(4)),
        );
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ChatChunk = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
        assert!(matches!(decoded.finish_reason, Some(FinishReason::Stop)));
        let usage = decoded.usage.unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 20);
    }

    #[test]
    fn chat_chunk_tool_call_round_trips() {
        for original in [
            ChatChunk::tool_call_start("call_1", "search"),
            ChatChunk::tool_call_arguments("call_1", "{\"q\":"),
            ChatChunk::tool_call_complete("call_1"),
            ChatChunk::usage(TokenUsage::new(1, 2)),
        ] {
            let encoded = codec::encode(&original).expect("encode");
            let decoded: ChatChunk = codec::decode(&encoded).expect("decode");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn model_info_round_trips() {
        let original = ModelInfo::new("b", "m", "M").with_capabilities(ModelCapabilities {
            streaming: true,
            tools: true,
            ..Default::default()
        });
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ModelInfo = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn chat_message_constructors_set_expected_fields() {
        let user = ChatMessage::user("hi");
        assert_eq!(user.role, ChatRole::User);
        assert_eq!(user.content, ChatContent::Text("hi".into()));
        assert!(user.tool_call_id.is_none());
        assert!(user.tool_calls.is_empty());

        let tool = ChatMessage::tool("call_1", "result");
        assert_eq!(tool.role, ChatRole::Tool);
        assert_eq!(tool.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn token_usage_builders_set_optional_fields() {
        let u = TokenUsage::new(7, 11);
        assert_eq!(u.input_tokens, 7);
        assert_eq!(u.output_tokens, 11);
        assert!(u.cached_tokens.is_none());
        assert!(u.reasoning_tokens.is_none());

        let u2 = TokenUsage::new(1, 2).with_cached(3).with_reasoning(4);
        assert_eq!(u2.cached_tokens, Some(3));
        assert_eq!(u2.reasoning_tokens, Some(4));
    }

    // ----- Schema locks -----

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

    #[test]
    fn schema_lock_chat_chunk_text() {
        let chunk = ChatChunk::text("hi");
        let encoded = codec::encode(&chunk).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        // {"delta": {"Text": "hi"}} — finish_reason/usage omitted via
        // skip_serializing_if.
        assert_eq!(
            hex, "81a564656c746181a454657874a26869",
            "ChatChunk schema changed — review consumer impact before updating this literal"
        );
    }
}
