//! Typed client for the LLM service.
//!
//! `chat` / `chat_stream` use a streaming response: each frame is an
//! independently encoded [`ChatChunk`]. There is no header frame — every
//! frame yielded is itself a typed chunk. The buffered [`chat`] helper
//! accumulates the full sequence into `Vec<ChatChunk>`; callers wanting a
//! flat string concatenate [`ChunkDelta::Text`] deltas themselves.
//!
//! `list_models`, `status`, `load_model`, and `unload_model` are buffered
//! single-frame ops. `list_models` takes no request body — the request
//! side is closed immediately via `finish()` (zero `write_chunk` calls).
//!
//! [`ChunkDelta::Text`]: wafer_block::wire::llm::ChunkDelta::Text

use wafer_block::{
    codec,
    wire::llm::{
        ChatChunk, ChatRequest, LoadModelRequest, ModelInfo, ModelStatus, StatusRequest,
        UnloadModelRequest,
    },
    ServiceOp, WaferError,
};

use super::common::{call, call_ack, call_no_body, decode_frame, open_buffered};
use crate::stream::ResponseStream;

const BLOCK: &str = "wafer-run/llm";

// ---------------------------------------------------------------------------
// Buffered ops
// ---------------------------------------------------------------------------

/// Buffered: run a chat completion, collecting every streamed
/// [`ChatChunk`] into a `Vec`.
///
/// Wire-level, the response is a stream of independently encoded chunks;
/// this helper drains the stream and decodes each frame. Callers wanting a
/// flat assistant message should concatenate `ChunkDelta::Text` deltas.
pub fn chat(request: &ChatRequest) -> Result<Vec<ChatChunk>, WaferError> {
    let mut stream = chat_stream(request)?;
    let mut chunks = Vec::new();
    while let Some(chunk) = stream.next_chunk()? {
        chunks.push(chunk);
    }
    Ok(chunks)
}

/// Buffered: list all known models across registered backends.
///
/// The op takes no request body — the request side is closed immediately
/// via `finish()` (zero `write_chunk` calls before the response is read).
pub fn list_models() -> Result<Vec<ModelInfo>, WaferError> {
    call_no_body(BLOCK, ServiceOp::LLM_LIST_MODELS, vec![])
}

/// Buffered: query a specific model's load state.
pub fn status(request: &StatusRequest) -> Result<ModelStatus, WaferError> {
    call(BLOCK, ServiceOp::LLM_STATUS, request)
}

/// Buffered: request that a model be loaded into memory.
///
/// Returns `()` once the backend acknowledges the request; loading itself
/// may continue asynchronously (use [`status`] to poll).
pub fn load_model(request: &LoadModelRequest) -> Result<(), WaferError> {
    call_ack(BLOCK, ServiceOp::LLM_LOAD_MODEL, request)
}

/// Buffered: request that a model be unloaded from memory.
pub fn unload_model(request: &UnloadModelRequest) -> Result<(), WaferError> {
    call_ack(BLOCK, ServiceOp::LLM_UNLOAD_MODEL, request)
}

// ---------------------------------------------------------------------------
// Streaming ops
// ---------------------------------------------------------------------------

/// Streaming: run a chat completion, returning an [`LlmChatStream`] that
/// yields decoded [`ChatChunk`] values as they arrive.
///
/// Each yielded [`ChatChunk`] carries a `ChunkDelta` which may be text,
/// tool-call deltas, or empty (terminator). Callers wanting flat assistant
/// text should concatenate `ChunkDelta::Text` payloads. Stream end is
/// signaled by `Ok(None)`; the final non-`None` chunk typically carries a
/// `finish_reason` and may carry `usage`.
pub fn chat_stream(request: &ChatRequest) -> Result<LlmChatStream, WaferError> {
    let req_bytes = codec::encode(request)?;
    let response_stream = open_buffered(BLOCK, ServiceOp::LLM_CHAT, &req_bytes)?;
    Ok(LlmChatStream {
        inner: response_stream,
    })
}

/// Streaming response wrapper for `LLM_CHAT`.
///
/// Each call to [`Self::next_chunk`] reads the next response frame from
/// the underlying stream and decodes it as a [`ChatChunk`]. There is no
/// header frame, so the very first chunk is already a typed value.
#[must_use = "response stream must be consumed via next_chunk"]
pub struct LlmChatStream {
    inner: ResponseStream,
}

impl LlmChatStream {
    /// Pull the next chat chunk from the stream.
    ///
    /// Returns `Ok(Some(chunk))` for each chunk frame, and `Ok(None)`
    /// once the response is fully delivered (end-of-stream). Each chunk's
    /// `ChunkDelta` may be text, tool-call deltas, or empty (terminator);
    /// callers wanting flat text concatenate `ChunkDelta::Text` payloads.
    /// The final non-`None` chunk typically carries `finish_reason` and may
    /// carry `usage`.
    pub fn next_chunk(&mut self) -> Result<Option<ChatChunk>, WaferError> {
        match self.inner.next_chunk()? {
            None => Ok(None),
            Some(bytes) => decode_frame(&bytes, "llm.chat chunk frame").map(Some),
        }
    }
}
