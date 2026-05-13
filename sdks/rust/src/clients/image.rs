//! Typed client for the image service.
//!
//! Mirrors `clients/llm.rs`, but targets the image service. All ops are
//! buffered single-frame: `generate`, `list_models`, `status`,
//! `unload_model`. Streaming progress for model loading
//! (`IMAGE_LOAD_MODEL`) is native-only — skill blocks running on wasm
//! don't observe load progress; they call `generate` directly and the
//! backend either has the model ready or surfaces a `BackendError`.

/// Re-export the wire types so callers can `use
/// wafer_sdk::clients::image::{ImageRequest, ...}` for both request
/// payloads and decoded responses. Mirrors `wafer_core::clients::image`'s
/// native API surface so skill-block code reads the same as native-block
/// code.
pub use wafer_block::wire::image::{
    GeneratedImage, ImageParams, ImageRequest, ImageResponse, ModelCapabilities, ModelInfo,
    ModelState, ModelStatus, StatusRequest, UnloadModelRequest,
};
use wafer_block::{codec, Message, ServiceOp, WaferError};

use super::common::{collect_single_frame, consume_ack, open_buffered};
use crate::stream::CallStream;

const BLOCK: &str = "wafer-run/image";

// ---------------------------------------------------------------------------
// Buffered ops
// ---------------------------------------------------------------------------

/// Buffered: generate an image from the given prompt and params.
///
/// Returns the decoded [`ImageResponse`] envelope; callers typically take
/// the first image: `resp.images.into_iter().next()`.
pub fn generate(request: &ImageRequest) -> Result<ImageResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::IMAGE_GENERATE, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "image GENERATE")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding image GENERATE response: {}", e.message),
        )
    })
}

/// Buffered: list every model exposed by every registered image backend.
///
/// The op takes no request body — the request side is closed immediately
/// via `finish()` (zero `write_chunk` calls before the response is read).
pub fn list_models() -> Result<Vec<ModelInfo>, WaferError> {
    let msg = Message {
        kind: ServiceOp::IMAGE_LIST_MODELS.to_string(),
        meta: vec![],
    };
    let call = CallStream::open(BLOCK, &msg)?;
    let mut response_stream = call.finish()?;
    let body = collect_single_frame(&mut response_stream, "image LIST_MODELS")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding image LIST_MODELS response: {}", e.message),
        )
    })
}

/// Buffered: query the current load state of `(backend_id, model_id)`.
pub fn status(request: &StatusRequest) -> Result<ModelStatus, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::IMAGE_STATUS, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "image STATUS")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding image STATUS response: {}", e.message),
        )
    })
}

/// Buffered: unload `(backend_id, model_id)`. Handler emits an empty ack
/// frame; this helper drops it.
pub fn unload_model(request: &UnloadModelRequest) -> Result<(), WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::IMAGE_UNLOAD_MODEL, &req_bytes)?;
    consume_ack(&mut response_stream)
}
