//! Typed native client for the `wafer-run/image` service block.
//!
//! Mirrors `clients/llm.rs`, but targets the image service. Two response
//! shapes:
//! - **Buffered** — `generate`, `list_models`, `status`, `unload_model`. Single
//!   decoded value (or unit), produced by [`super::call_service`].
//! - **Streaming** — `load_model_stream`. Each response frame is one
//!   independently encoded wire value; the typed wrapper decodes per-frame.

#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
/// Re-export the wire types so callers use one path for both the request
/// payloads and the typed stream items.
pub use wafer_block::wire::image::{
    GeneratedImage, ImageParams, ImageRequest, ImageResponse, LoadModelRequest, LoadProgress,
    ModelCapabilities, ModelInfo, ModelState, ModelStatus, StatusRequest, UnloadModelRequest,
};
use wafer_block::{common::ServiceOp, WaferError};

use super::{call_service, decode};
#[cfg(not(feature = "wasm-component"))]
use super::{call_service_streaming, NativeTypedFrameStream};

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

/// Buffered: generate an image from the given prompt and params (WASM sync variant).
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

/// Buffered: list every model exposed by every registered image backend (WASM sync variant).
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

/// Buffered: query the current load state of `(backend_id, model_id)` (WASM sync variant).
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

/// Buffered: unload `(backend_id, model_id)` (WASM sync variant). Drops the response body.
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
