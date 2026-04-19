//! LlmService trait + shared data types.
//!
//! Mirrors the layout of `interfaces::database::service` — types and error live
//! here; the trait definition will follow in a subsequent task. See
//! solobase spec `2026-04-15-llm-service-refactor-design.md`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

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
}
