//! Typed client for the LLM service.
//!
//! `chat` / `chat_stream` use a streaming response: each frame is an
//! independently encoded [`ChatChunk`]. There is no header frame — every
//! frame yielded is itself a typed chunk. The buffered [`chat`] helper
//! accumulates the full sequence into `Vec<ChatChunk>`; callers wanting a
//! flat string concatenate [`ChunkDelta::Text`] deltas themselves.
//!
//! `list_models`, `status`, `load_model`, and `unload_model` are buffered
//! single-frame ops. `list_models` takes no request — an empty request body
//! is sent (zero `write_chunk` calls before `finish`).

use wafer_block::{
    codec,
    wire::llm::{
        ChatChunk, ChatRequest, LoadModelRequest, ModelInfo, ModelStatus, StatusRequest,
        UnloadModelRequest,
    },
    ErrorCode, Message, ServiceOp, WaferError,
};

use crate::stream::{CallStream, ResponseStream};

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
/// The op takes no arguments — an empty request body is sent (the
/// underlying [`CallStream`] is opened and immediately finished without a
/// `write_chunk`).
pub fn list_models() -> Result<Vec<ModelInfo>, WaferError> {
    let msg = Message {
        kind: ServiceOp::LLM_LIST_MODELS.to_string(),
        meta: vec![],
    };
    let call = CallStream::open(BLOCK, &msg)?;
    let mut response_stream = call.finish()?;
    let body = collect_single_frame(&mut response_stream, "llm LIST_MODELS")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding llm LIST_MODELS response: {}", e.message),
        )
    })
}

/// Buffered: query a specific model's load state.
pub fn status(request: &StatusRequest) -> Result<ModelStatus, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(ServiceOp::LLM_STATUS, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "llm STATUS")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding llm STATUS response: {}", e.message),
        )
    })
}

/// Buffered: request that a model be loaded into memory.
///
/// Returns `()` once the backend acknowledges the request; loading itself
/// may continue asynchronously (use [`status`] to poll).
pub fn load_model(request: &LoadModelRequest) -> Result<(), WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(ServiceOp::LLM_LOAD_MODEL, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: request that a model be unloaded from memory.
pub fn unload_model(request: &UnloadModelRequest) -> Result<(), WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(ServiceOp::LLM_UNLOAD_MODEL, &req_bytes)?;
    consume_ack(&mut response_stream)
}

// ---------------------------------------------------------------------------
// Streaming ops
// ---------------------------------------------------------------------------

/// Streaming: run a chat completion, returning an [`LlmChatStream`] that
/// yields decoded [`ChatChunk`] values as they arrive.
pub fn chat_stream(request: &ChatRequest) -> Result<LlmChatStream, WaferError> {
    let req_bytes = codec::encode(request)?;
    let response_stream = open_buffered(ServiceOp::LLM_CHAT, &req_bytes)?;
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
    /// once the response is fully delivered (end-of-stream). Each frame is
    /// decoded into a [`ChatChunk`] before being returned, so callers
    /// receive typed values directly rather than raw bytes.
    pub fn next_chunk(&mut self) -> Result<Option<ChatChunk>, WaferError> {
        match self.inner.next_chunk()? {
            None => Ok(None),
            Some(bytes) => {
                let chunk: ChatChunk = codec::decode(&bytes).map_err(|e| {
                    WaferError::new(
                        e.code,
                        format!("decoding llm chat chunk frame: {}", e.message),
                    )
                })?;
                Ok(Some(chunk))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Send a single-frame request and return the response stream.
fn open_buffered(op: &str, req_bytes: &[u8]) -> Result<ResponseStream, WaferError> {
    let msg = Message {
        kind: op.to_string(),
        meta: vec![],
    };
    let mut call = CallStream::open(BLOCK, &msg)?;
    call.write_chunk(req_bytes)?;
    call.finish()
}

/// Drain a single-frame ack response. The frame's payload is ignored
/// (handlers may choose to send an empty body); end-of-stream is then
/// asserted before returning.
fn consume_ack(response_stream: &mut ResponseStream) -> Result<(), WaferError> {
    let _ = response_stream.next_chunk()?;
    while response_stream.next_chunk()?.is_some() {}
    Ok(())
}

/// Collect a single response frame whose payload carries an encoded value.
fn collect_single_frame(
    response_stream: &mut ResponseStream,
    context: &str,
) -> Result<Vec<u8>, WaferError> {
    let body = response_stream.next_chunk()?.ok_or_else(|| {
        WaferError::new(
            ErrorCode::Internal,
            format!("{context}: stream ended before response frame"),
        )
    })?;
    while response_stream.next_chunk()?.is_some() {}
    Ok(body)
}
