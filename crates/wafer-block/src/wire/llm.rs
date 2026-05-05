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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub backend_id: String,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub params: ChatParams,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatParams {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    pub seed: Option<u64>,
    pub response_format: Option<ResponseFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseFormat {
    Text,
    Json,
    JsonSchema(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: ChatContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChatContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContentPart {
    Text(String),
    ImageUrl { url: String, detail: Option<String> },
    ImageBytes { bytes: Vec<u8>, mime_type: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

// ---- Chat streaming chunk ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChunk {
    pub delta: ChunkDelta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChunkDelta {
    Text(String),
    ToolCallStart { id: String, name: String },
    ToolCallArguments { id: String, arguments_delta: String },
    ToolCallComplete { id: String },
    Empty,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCall,
    ContentFilter,
    Error,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: Option<u32>,
    pub reasoning_tokens: Option<u32>,
}

// ---- Status / model management ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusRequest {
    pub backend_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadModelRequest {
    pub backend_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnloadModelRequest {
    pub backend_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub backend_id: String,
    pub model_id: String,
    pub display_name: String,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub streaming: bool,
    pub tools: bool,
    pub vision: bool,
    pub json_mode: bool,
    pub max_context_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatus {
    pub state: ModelState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelState {
    Ready,
    Loading,
    Unloaded,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadProgress {
    pub stage: String,
    pub bytes_downloaded: Option<u64>,
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
