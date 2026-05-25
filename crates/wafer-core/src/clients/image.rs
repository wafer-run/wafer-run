//! Typed native client for the `wafer-run/image` service block.
//!
//! Mirrors `clients/llm.rs`, but targets the image service. Two response
//! shapes:
//! - **Buffered** — `generate`, `list_models`, `status`, `unload_model`. Single
//!   decoded value (or unit), produced by [`super::call_service`].
//! - **Streaming** — `load_model_stream`. Each response frame is one
//!   independently encoded wire value; the typed wrapper decodes per-frame.

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
pub use wafer_block::wire::image::{
    GeneratedImage, ImageParams, ImageRequest, ImageResponse, LoadModelRequest, LoadProgress,
    ModelCapabilities, ModelInfo, ModelState, ModelStatus, StatusRequest, UnloadModelRequest,
};
#[cfg(not(feature = "wasm-component"))]
use wafer_block::{codec, common::ErrorCode};
use wafer_block::{common::ServiceOp, WaferError};

#[cfg(not(feature = "wasm-component"))]
use super::call_service_streaming;
use super::{call_service, decode};

const BLOCK: &str = "wafer-run/image";

// ===========================================================================
// Buffered ops
// ===========================================================================

/// Buffered: generate an image from the given prompt and params.
#[cfg(not(feature = "wasm-component"))]
pub async fn generate(
    ctx: &dyn Context,
    request: &ImageRequest,
) -> Result<ImageResponse, WaferError> {
    let body = call_service(
        ctx,
        BLOCK,
        ServiceOp::IMAGE_GENERATE,
        request,
        None,
        false,
        None,
    )
    .await?;
    decode(&body)
}

#[cfg(feature = "wasm-component")]
pub fn generate(request: &ImageRequest) -> Result<ImageResponse, WaferError> {
    let body = call_service(BLOCK, ServiceOp::IMAGE_GENERATE, request, None, false, None)?;
    decode(&body)
}

/// Buffered: list every model exposed by every registered image backend.
///
/// `IMAGE_LIST_MODELS` takes no request body — the typed client encodes a unit
/// value `()`, which becomes a zero-byte MessagePack `nil` frame.
#[cfg(not(feature = "wasm-component"))]
pub async fn list_models(ctx: &dyn Context) -> Result<Vec<ModelInfo>, WaferError> {
    let body = call_service(
        ctx,
        BLOCK,
        ServiceOp::IMAGE_LIST_MODELS,
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
    let body = call_service(BLOCK, ServiceOp::IMAGE_LIST_MODELS, &(), None, false, None)?;
    decode(&body)
}

/// Buffered: query the current load state of `(backend_id, model_id)`.
#[cfg(not(feature = "wasm-component"))]
pub async fn status(ctx: &dyn Context, request: &StatusRequest) -> Result<ModelStatus, WaferError> {
    let body = call_service(
        ctx,
        BLOCK,
        ServiceOp::IMAGE_STATUS,
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
    let body = call_service(BLOCK, ServiceOp::IMAGE_STATUS, request, None, false, None)?;
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
        ServiceOp::IMAGE_UNLOAD_MODEL,
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
        ServiceOp::IMAGE_UNLOAD_MODEL,
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
        ServiceOp::IMAGE_LOAD_MODEL,
        request,
        None,
        true,
        None,
    )
    .await?;
    Ok(NativeTypedFrameStream::new(out, "image load_model"))
}

// ===========================================================================
// Streaming wrapper
// ===========================================================================

/// Stream wrapper that decodes each `Chunk` frame as `T` via [`codec::decode`].
///
/// Used by services whose response is a sequence of independently encoded
/// values with no header frame. Non-`Complete` terminals are translated into a
/// single `Err` item, after which the stream returns `None`.
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
                Poll::Ready(Some(StreamEvent::Halt { .. })) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(WaferError::new(
                        ErrorCode::INTERNAL,
                        format!("{}: handler returned Halt", self.context),
                    ))));
                }
            }
        }
    }
}
