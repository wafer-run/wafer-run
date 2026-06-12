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
use wafer_block::{ServiceOp, WaferError};

use super::common::{call, call_ack, call_no_body};

const BLOCK: &str = "wafer-run/image";

/// Buffered: generate an image from the given prompt and params.
///
/// Returns the decoded [`ImageResponse`] envelope; callers typically take
/// the first image: `resp.images.into_iter().next()`.
pub fn generate(request: &ImageRequest) -> Result<ImageResponse, WaferError> {
    call(BLOCK, ServiceOp::IMAGE_GENERATE, request)
}

/// Buffered: list every model exposed by every registered image backend.
///
/// The op takes no request body — the request side is closed immediately
/// via `finish()` (zero `write_chunk` calls before the response is read).
pub fn list_models() -> Result<Vec<ModelInfo>, WaferError> {
    call_no_body(BLOCK, ServiceOp::IMAGE_LIST_MODELS, vec![])
}

/// Buffered: query the current load state of `(backend_id, model_id)`.
pub fn status(request: &StatusRequest) -> Result<ModelStatus, WaferError> {
    call(BLOCK, ServiceOp::IMAGE_STATUS, request)
}

/// Buffered: unload `(backend_id, model_id)`. Handler emits an empty ack
/// frame; this helper drops it.
pub fn unload_model(request: &UnloadModelRequest) -> Result<(), WaferError> {
    call_ack(BLOCK, ServiceOp::IMAGE_UNLOAD_MODEL, request)
}
