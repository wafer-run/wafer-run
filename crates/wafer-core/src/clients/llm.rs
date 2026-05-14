//! Typed native client for the `wafer-run/llm` service block.
//!
//! Mirrors the SDK-side client at `sdks/rust/src/clients/llm.rs`, but
//! against the runtime [`Context`] rather than the WASM call-stream ABI.
//!
//! Two response shapes:
//! - **Buffered** — `list_models`, `status`, `unload_model`. Single decoded
//!   value (or unit), produced by [`super::call_service`].
//! - **Streaming** — `chat_stream`, `load_model_stream`. Each response frame
//!   is one independently encoded wire value; the typed wrapper decodes
//!   per-frame. There is no header frame, unlike `clients::network`.

#[cfg(not(feature = "wasm-component"))]
use std::marker::PhantomData;
#[cfg(not(feature = "wasm-component"))]
use std::pin::Pin;
#[cfg(not(feature = "wasm-component"))]
use std::task::{Context as TaskContext, Poll};

#[cfg(not(feature = "wasm-component"))]
use futures::Stream;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::stream::StreamEvent;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::streams::output::OutputStream;
/// Re-export the wire types so callers use one path for both the request
/// payloads and the typed stream items.
pub use wafer_block::wire::llm::{
    ChatChunk, ChatContent, ChatMessage, ChatParams, ChatRequest, ChatRole, ChunkDelta,
    ContentPart, FinishReason, LoadModelRequest, LoadProgress, ModelCapabilities, ModelInfo,
    ModelState, ModelStatus, ResponseFormat, StatusRequest, TokenUsage, ToolCall, ToolDefinition,
    UnloadModelRequest,
};
#[cfg(not(feature = "wasm-component"))]
use wafer_block::{codec, common::ErrorCode};
use wafer_block::{common::ServiceOp, WaferError};

#[cfg(not(feature = "wasm-component"))]
use super::call_service_streaming;
use super::{call_service, decode};

const BLOCK: &str = "wafer-run/llm";

// ===========================================================================
// Buffered ops
// ===========================================================================

/// Buffered: list every model exposed by every registered LLM backend.
///
/// `LLM_LIST_MODELS` takes no request body — the typed client encodes a unit
/// value `()`, which becomes a zero-byte MessagePack `nil` frame.
#[cfg(not(feature = "wasm-component"))]
pub async fn list_models(ctx: &dyn Context) -> Result<Vec<ModelInfo>, WaferError> {
    let body = call_service(
        ctx,
        BLOCK,
        ServiceOp::LLM_LIST_MODELS,
        &(),
        None,
        false,
        None,
    )
    .await?;
    decode(&body)
}

#[cfg(feature = "wasm-component")]
pub fn list_models() -> Result<Vec<ModelInfo>, WaferError> {
    let body = call_service(BLOCK, ServiceOp::LLM_LIST_MODELS, &(), None, false, None)?;
    decode(&body)
}

/// Buffered: query the current load state of `(backend_id, model_id)`.
#[cfg(not(feature = "wasm-component"))]
pub async fn status(ctx: &dyn Context, request: &StatusRequest) -> Result<ModelStatus, WaferError> {
    let body = call_service(
        ctx,
        BLOCK,
        ServiceOp::LLM_STATUS,
        request,
        None,
        false,
        None,
    )
    .await?;
    decode(&body)
}

#[cfg(feature = "wasm-component")]
pub fn status(request: &StatusRequest) -> Result<ModelStatus, WaferError> {
    let body = call_service(BLOCK, ServiceOp::LLM_STATUS, request, None, false, None)?;
    decode(&body)
}

/// Buffered: unload `(backend_id, model_id)`. Drops the response body — the
/// handler emits an empty ack frame.
#[cfg(not(feature = "wasm-component"))]
pub async fn unload_model(
    ctx: &dyn Context,
    request: &UnloadModelRequest,
) -> Result<(), WaferError> {
    call_service(
        ctx,
        BLOCK,
        ServiceOp::LLM_UNLOAD_MODEL,
        request,
        None,
        true,
        None,
    )
    .await?;
    Ok(())
}

#[cfg(feature = "wasm-component")]
pub fn unload_model(request: &UnloadModelRequest) -> Result<(), WaferError> {
    call_service(
        BLOCK,
        ServiceOp::LLM_UNLOAD_MODEL,
        request,
        None,
        true,
        None,
    )?;
    Ok(())
}

// ===========================================================================
// Streaming ops (native only)
// ===========================================================================

/// Native streaming: run a chat completion, returning a stream that yields
/// decoded [`ChatChunk`] frames as the backend produces them.
///
/// Each frame is independently MessagePack-encoded; there is no header frame.
/// The final non-`None` chunk typically carries `finish_reason` and may carry
/// `usage`. Stream end is signalled by the wrapper returning `Poll::Ready(None)`.
#[cfg(not(feature = "wasm-component"))]
pub async fn chat_stream(
    ctx: &dyn Context,
    request: &ChatRequest,
) -> Result<NativeTypedFrameStream<ChatChunk>, WaferError> {
    let out =
        call_service_streaming(ctx, BLOCK, ServiceOp::LLM_CHAT, request, None, false, None).await?;
    Ok(NativeTypedFrameStream::new(out, "llm chat"))
}

/// Native buffered: convenience wrapper that drains [`chat_stream`] and
/// collects every chunk into a `Vec`. Callers wanting flat assistant text
/// concatenate `ChunkDelta::Text` deltas themselves.
#[cfg(not(feature = "wasm-component"))]
pub async fn chat(ctx: &dyn Context, request: &ChatRequest) -> Result<Vec<ChatChunk>, WaferError> {
    use futures::StreamExt;
    let mut stream = chat_stream(ctx, request).await?;
    let mut chunks = Vec::new();
    while let Some(item) = stream.next().await {
        chunks.push(item?);
    }
    Ok(chunks)
}

/// Native streaming: start loading `(backend_id, model_id)` into memory and
/// return a stream of [`LoadProgress`] frames reporting backend-defined
/// progress milestones.
#[cfg(not(feature = "wasm-component"))]
pub async fn load_model_stream(
    ctx: &dyn Context,
    request: &LoadModelRequest,
) -> Result<NativeTypedFrameStream<LoadProgress>, WaferError> {
    let out = call_service_streaming(
        ctx,
        BLOCK,
        ServiceOp::LLM_LOAD_MODEL,
        request,
        None,
        true,
        None,
    )
    .await?;
    Ok(NativeTypedFrameStream::new(out, "llm load_model"))
}

// ===========================================================================
// Streaming wrapper
// ===========================================================================

/// Stream wrapper that decodes each `Chunk` frame as `T` via [`codec::decode`].
///
/// Used by services whose response is a sequence of independently encoded
/// values with no header frame (LLM `chat`, `load_model`). Non-`Complete`
/// terminals are translated into a single `Err` item, after which the stream
/// returns `None`.
#[cfg(not(feature = "wasm-component"))]
#[must_use = "response stream must be consumed"]
pub struct NativeTypedFrameStream<T> {
    inner: OutputStream,
    context: &'static str,
    finished: bool,
    _frame: PhantomData<T>,
}

#[cfg(not(feature = "wasm-component"))]
impl<T> NativeTypedFrameStream<T> {
    fn new(inner: OutputStream, context: &'static str) -> Self {
        Self {
            inner,
            context,
            finished: false,
            _frame: PhantomData,
        }
    }
}

#[cfg(not(feature = "wasm-component"))]
impl<T> Stream for NativeTypedFrameStream<T>
where
    T: serde::de::DeserializeOwned + Unpin,
{
    type Item = Result<T, WaferError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        if self.finished {
            return Poll::Ready(None);
        }
        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(WaferError::new(
                        ErrorCode::INTERNAL,
                        format!("{}: stream ended without terminal event", self.context),
                    ))));
                }
                Poll::Ready(Some(StreamEvent::Chunk(bytes))) => {
                    let ctx_label = self.context;
                    return Poll::Ready(Some(codec::decode::<T>(&bytes).map_err(|e| {
                        WaferError::new(e.code, format!("{ctx_label} frame decode: {}", e.message))
                    })));
                }
                Poll::Ready(Some(StreamEvent::Meta(_))) => continue,
                Poll::Ready(Some(StreamEvent::Complete { .. })) => {
                    self.finished = true;
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(StreamEvent::Error(e))) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(*e)));
                }
                Poll::Ready(Some(StreamEvent::Drop)) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(WaferError::new(
                        ErrorCode::INTERNAL,
                        format!("{}: handler returned Drop", self.context),
                    ))));
                }
                Poll::Ready(Some(StreamEvent::Continue(msg))) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(WaferError::new(
                        ErrorCode::INTERNAL,
                        format!(
                            "{}: handler returned Continue (kind: {})",
                            self.context, msg.kind
                        ),
                    ))));
                }
            }
        }
    }
}
