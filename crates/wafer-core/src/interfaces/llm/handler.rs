//! Shared message handler for LLM blocks.
//!
//! Decodes `msg.kind` into calls on a `LlmService` impl and translates the
//! result onto an `OutputStream`. Buffered ops (`list_models`, `status`,
//! `unload_model`) produce a single `respond(body)`. Streaming ops (`chat`,
//! `load_model`) produce a `from_producer` stream: each service chunk is
//! codec-encoded (MessagePack) and emitted as its own `Chunk` event, and
//! cancellation from the consumer is forwarded straight through to the
//! service's cancel token.

use std::sync::Arc;

use futures::StreamExt;
use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    wire::llm as wire,
    *,
};

use super::service::{self, LlmService};

// ---------- Wire <-> service conversions ----------
//
// Field-identical types — conversion is mechanical. We keep them in the
// handler rather than implementing `From` on the service crate because the
// service trait is intentionally agnostic of the wire representation.

fn wire_chat_request_to_service(req: wire::ChatRequest) -> service::ChatRequest {
    service::ChatRequest {
        backend_id: req.backend_id,
        model: req.model,
        messages: req.messages.into_iter().map(wire_msg_to_service).collect(),
        params: wire_params_to_service(req.params),
        tools: req
            .tools
            .into_iter()
            .map(wire_tool_def_to_service)
            .collect(),
        extra: req.extra,
    }
}

fn wire_msg_to_service(m: wire::ChatMessage) -> service::ChatMessage {
    service::ChatMessage {
        role: wire_role_to_service(m.role),
        content: wire_content_to_service(m.content),
        tool_call_id: m.tool_call_id,
        tool_calls: m
            .tool_calls
            .into_iter()
            .map(wire_tool_call_to_service)
            .collect(),
    }
}

fn wire_role_to_service(r: wire::ChatRole) -> service::ChatRole {
    match r {
        wire::ChatRole::System => service::ChatRole::System,
        wire::ChatRole::User => service::ChatRole::User,
        wire::ChatRole::Assistant => service::ChatRole::Assistant,
        wire::ChatRole::Tool => service::ChatRole::Tool,
    }
}

fn wire_content_to_service(c: wire::ChatContent) -> service::ChatContent {
    match c {
        wire::ChatContent::Text(s) => service::ChatContent::Text(s),
        wire::ChatContent::Parts(parts) => {
            service::ChatContent::Parts(parts.into_iter().map(wire_part_to_service).collect())
        }
    }
}

fn wire_part_to_service(p: wire::ContentPart) -> service::ContentPart {
    match p {
        wire::ContentPart::Text(s) => service::ContentPart::Text(s),
        wire::ContentPart::ImageUrl { url, detail } => {
            service::ContentPart::ImageUrl { url, detail }
        }
        wire::ContentPart::ImageBytes { bytes, mime_type } => {
            service::ContentPart::ImageBytes { bytes, mime_type }
        }
    }
}

fn wire_tool_call_to_service(t: wire::ToolCall) -> service::ToolCall {
    service::ToolCall {
        id: t.id,
        name: t.name,
        arguments: t.arguments,
    }
}

fn wire_tool_def_to_service(t: wire::ToolDefinition) -> service::ToolDefinition {
    service::ToolDefinition {
        name: t.name,
        description: t.description,
        parameters: t.parameters,
    }
}

fn wire_params_to_service(p: wire::ChatParams) -> service::ChatParams {
    service::ChatParams {
        max_tokens: p.max_tokens,
        temperature: p.temperature,
        top_p: p.top_p,
        stop_sequences: p.stop_sequences,
        seed: p.seed,
        response_format: p.response_format.map(wire_response_format_to_service),
    }
}

fn wire_response_format_to_service(r: wire::ResponseFormat) -> service::ResponseFormat {
    match r {
        wire::ResponseFormat::Text => service::ResponseFormat::Text,
        wire::ResponseFormat::Json => service::ResponseFormat::Json,
        wire::ResponseFormat::JsonSchema(v) => service::ResponseFormat::JsonSchema(v),
    }
}

fn service_chat_chunk_to_wire(c: service::ChatChunk) -> wire::ChatChunk {
    wire::ChatChunk {
        delta: service_chunk_delta_to_wire(c.delta),
        finish_reason: c.finish_reason.map(service_finish_reason_to_wire),
        usage: c.usage.map(service_token_usage_to_wire),
    }
}

fn service_chunk_delta_to_wire(d: service::ChunkDelta) -> wire::ChunkDelta {
    match d {
        service::ChunkDelta::Text(s) => wire::ChunkDelta::Text(s),
        service::ChunkDelta::ToolCallStart { id, name } => {
            wire::ChunkDelta::ToolCallStart { id, name }
        }
        service::ChunkDelta::ToolCallArguments {
            id,
            arguments_delta,
        } => wire::ChunkDelta::ToolCallArguments {
            id,
            arguments_delta,
        },
        service::ChunkDelta::ToolCallComplete { id } => wire::ChunkDelta::ToolCallComplete { id },
        service::ChunkDelta::Empty => wire::ChunkDelta::Empty,
    }
}

fn service_finish_reason_to_wire(r: service::FinishReason) -> wire::FinishReason {
    match r {
        service::FinishReason::Stop => wire::FinishReason::Stop,
        service::FinishReason::Length => wire::FinishReason::Length,
        service::FinishReason::ToolCall => wire::FinishReason::ToolCall,
        service::FinishReason::ContentFilter => wire::FinishReason::ContentFilter,
        service::FinishReason::Error => wire::FinishReason::Error,
    }
}

fn service_token_usage_to_wire(u: service::TokenUsage) -> wire::TokenUsage {
    wire::TokenUsage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cached_tokens: u.cached_tokens,
        reasoning_tokens: u.reasoning_tokens,
    }
}

fn service_model_info_to_wire(m: service::ModelInfo) -> wire::ModelInfo {
    wire::ModelInfo {
        backend_id: m.backend_id,
        model_id: m.model_id,
        display_name: m.display_name,
        capabilities: service_caps_to_wire(m.capabilities),
    }
}

fn service_caps_to_wire(c: service::ModelCapabilities) -> wire::ModelCapabilities {
    wire::ModelCapabilities {
        streaming: c.streaming,
        tools: c.tools,
        vision: c.vision,
        json_mode: c.json_mode,
        max_context_tokens: c.max_context_tokens,
        max_output_tokens: c.max_output_tokens,
    }
}

fn service_model_status_to_wire(s: service::ModelStatus) -> wire::ModelStatus {
    wire::ModelStatus {
        state: service_model_state_to_wire(s.state),
        progress: s.progress,
    }
}

fn service_model_state_to_wire(s: service::ModelState) -> wire::ModelState {
    match s {
        service::ModelState::Ready => wire::ModelState::Ready,
        service::ModelState::Loading => wire::ModelState::Loading,
        service::ModelState::Unloaded => wire::ModelState::Unloaded,
        service::ModelState::Error { message } => wire::ModelState::Error { message },
    }
}

fn service_load_progress_to_wire(p: service::LoadProgress) -> wire::LoadProgress {
    wire::LoadProgress {
        stage: p.stage,
        bytes_downloaded: p.bytes_downloaded,
        bytes_total: p.bytes_total,
    }
}

// ---------- Entry point ----------

/// Dispatch an `llm.*` message to the appropriate handler on `service` and
/// return the resulting output stream. Unknown ops yield an `INVALID_ARGUMENT`
/// error stream.
pub async fn handle_message(
    service: Arc<dyn LlmService>,
    msg: &Message,
    body: Vec<u8>,
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::LLM_CHAT => chat(service, body),
        ServiceOp::LLM_LIST_MODELS => list_models(service).await,
        ServiceOp::LLM_STATUS => status(service, &body).await,
        ServiceOp::LLM_LOAD_MODEL => load_model(service, &body),
        ServiceOp::LLM_UNLOAD_MODEL => unload_model(service, &body).await,
        other => OutputStream::error(WaferError::new(
            ErrorCode::INVALID_ARGUMENT,
            format!("unknown llm operation: {other}"),
        )),
    }
}

// ---- Streaming ops ----

fn chat(service: Arc<dyn LlmService>, body: Vec<u8>) -> OutputStream {
    // Decode up front — failures become an error stream rather than a malformed
    // chunk halfway through.
    let wire_req: wire::ChatRequest = match codec::decode(&body) {
        Ok(r) => r,
        Err(e) => {
            return OutputStream::error(WaferError::new(
                ErrorCode::INVALID_ARGUMENT,
                format!("invalid llm.chat request: {}", e.message),
            ));
        }
    };
    let req = wire_chat_request_to_service(wire_req);

    OutputStream::from_producer(move |sink, cancel| async move {
        let mut stream = service.chat_stream(req, cancel).await;
        while let Some(item) = stream.next().await {
            // Each frame is a `wire::ChatChunk` directly. Service-level
            // `LlmError` is surfaced as a terminal stream error rather than
            // a Result-wrapped chunk, matching the SDK's `next_chunk` decode
            // (which treats every frame as a `ChatChunk`).
            let chunk = match item {
                Ok(c) => service_chat_chunk_to_wire(c),
                Err(e) => {
                    let _ = sink
                        .error(WaferError::new(
                            ErrorCode::INTERNAL,
                            format!("llm.chat: {e}"),
                        ))
                        .await;
                    return;
                }
            };
            let bytes = match codec::encode(&chunk) {
                Ok(b) => b,
                Err(e) => {
                    let _ = sink
                        .error(WaferError::new(
                            ErrorCode::INTERNAL,
                            format!("encoding chat chunk: {}", e.message),
                        ))
                        .await;
                    return;
                }
            };
            if sink.send_chunk(bytes).await.is_err() {
                // Consumer dropped — cancel token has already fired via
                // OutputStream::drop, which from_producer wires through.
                return;
            }
        }
        // Natural end of stream: auto-complete when sink drops.
    })
}

fn load_model(service: Arc<dyn LlmService>, body: &[u8]) -> OutputStream {
    let req: wire::LoadModelRequest = match codec::decode(body) {
        Ok(r) => r,
        Err(e) => {
            return OutputStream::error(WaferError::new(
                ErrorCode::INVALID_ARGUMENT,
                format!("invalid llm.load_model request: {}", e.message),
            ));
        }
    };

    OutputStream::from_producer(move |sink, cancel| async move {
        let mut stream = service.load_model(&req.backend_id, &req.model_id, cancel);
        while let Some(item) = stream.next().await {
            let progress = match item {
                Ok(p) => service_load_progress_to_wire(p),
                Err(e) => {
                    let _ = sink
                        .error(WaferError::new(
                            ErrorCode::INTERNAL,
                            format!("llm.load_model: {e}"),
                        ))
                        .await;
                    return;
                }
            };
            let bytes = match codec::encode(&progress) {
                Ok(b) => b,
                Err(e) => {
                    let _ = sink
                        .error(WaferError::new(
                            ErrorCode::INTERNAL,
                            format!("encoding load progress: {}", e.message),
                        ))
                        .await;
                    return;
                }
            };
            if sink.send_chunk(bytes).await.is_err() {
                return;
            }
        }
    })
}

// ---- Buffered ops ----

async fn list_models(service: Arc<dyn LlmService>) -> OutputStream {
    match service.list_models().await {
        Ok(models) => {
            let wire_models: Vec<wire::ModelInfo> =
                models.into_iter().map(service_model_info_to_wire).collect();
            match codec::encode(&wire_models) {
                Ok(bytes) => OutputStream::respond(bytes),
                Err(e) => OutputStream::error(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("encoding list_models response: {}", e.message),
                )),
            }
        }
        Err(e) => OutputStream::error(WaferError::new(
            ErrorCode::INTERNAL,
            format!("list_models: {e}"),
        )),
    }
}

async fn status(service: Arc<dyn LlmService>, body: &[u8]) -> OutputStream {
    let req: wire::StatusRequest = match codec::decode(body) {
        Ok(r) => r,
        Err(e) => {
            return OutputStream::error(WaferError::new(
                ErrorCode::INVALID_ARGUMENT,
                format!("invalid llm.status request: {}", e.message),
            ));
        }
    };
    match service.status(&req.backend_id, &req.model_id).await {
        Ok(s) => {
            let wire_status = service_model_status_to_wire(s);
            match codec::encode(&wire_status) {
                Ok(bytes) => OutputStream::respond(bytes),
                Err(e) => OutputStream::error(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("encoding status response: {}", e.message),
                )),
            }
        }
        Err(e) => OutputStream::error(WaferError::new(ErrorCode::INTERNAL, format!("status: {e}"))),
    }
}

async fn unload_model(service: Arc<dyn LlmService>, body: &[u8]) -> OutputStream {
    let req: wire::UnloadModelRequest = match codec::decode(body) {
        Ok(r) => r,
        Err(e) => {
            return OutputStream::error(WaferError::new(
                ErrorCode::INVALID_ARGUMENT,
                format!("invalid llm.unload_model request: {}", e.message),
            ));
        }
    };
    match service.unload_model(&req.backend_id, &req.model_id).await {
        Ok(()) => OutputStream::respond(vec![]),
        Err(e) => OutputStream::error(WaferError::new(
            ErrorCode::INTERNAL,
            format!("unload_model: {e}"),
        )),
    }
}
