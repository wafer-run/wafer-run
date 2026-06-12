//! Shared message handler for image blocks.
//!
//! Decodes `msg.kind` into calls on an `ImageService` impl and translates the
//! result onto an `OutputStream`. Buffered ops (`generate`, `list_models`,
//! `status`, `unload_model`) produce a single `respond(body)`. The streaming
//! op (`load_model`) produces a `from_producer` stream: each service chunk is
//! codec-encoded (MessagePack) and emitted as its own `Chunk` event, and
//! cancellation from the consumer is forwarded straight through to the
//! service's cancel token.
//!
//! The service trait operates directly on the `wafer_block::wire::image`
//! types (re-exported from `super::service`), so request/response values pass
//! straight through — only `ImageError` needs mapping onto wire error codes.

use std::sync::Arc;

use futures::StreamExt;
use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    wire::image as wire,
    *,
};

use super::service::{ImageError, ImageService};
use crate::interfaces::handler_util::{decode_or_err, to_output};

fn image_error_to_block_error(e: ImageError) -> (ErrorCode, String) {
    match e {
        ImageError::NotSupported => (ErrorCode::Unimplemented, "not supported".to_string()),
        ImageError::InvalidRequest(msg) => (ErrorCode::InvalidArgument, msg),
        ImageError::BackendError(msg) => (ErrorCode::Internal, msg),
        ImageError::ModelNotFound(msg) => (ErrorCode::NotFound, msg),
        ImageError::Network(msg) => (ErrorCode::Internal, format!("network: {msg}")),
        ImageError::Cancelled => (ErrorCode::Cancelled, "cancelled".to_string()),
    }
}

// ---------- Entry point ----------

/// Dispatch an `image.*` message to the appropriate handler on `service` and
/// return the resulting output stream. Unknown ops yield an `INVALID_ARGUMENT`
/// error stream.
///
/// `service` is borrowed; the streaming op (`load_model`) clones the `Arc`
/// internally because its producer closure must be `'static`. Buffered ops
/// just borrow it.
pub async fn handle_message(
    service: &Arc<dyn ImageService>,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::IMAGE_GENERATE => generate(service.as_ref(), body).await,
        ServiceOp::IMAGE_LIST_MODELS => list_models(service.as_ref()).await,
        ServiceOp::IMAGE_STATUS => status(service.as_ref(), body).await,
        ServiceOp::IMAGE_LOAD_MODEL => load_model(service, body),
        ServiceOp::IMAGE_UNLOAD_MODEL => unload_model(service.as_ref(), body).await,
        other => OutputStream::error(WaferError::new(
            ErrorCode::InvalidArgument,
            format!("unknown image operation: {other}"),
        )),
    }
}

// ---- Buffered ops ----

async fn generate(service: &dyn ImageService, body: &[u8]) -> OutputStream {
    let req = decode_or_err!(body, wire::ImageRequest, "image.generate");
    // `generate` is not streaming — the whole response arrives at once.
    // Use a fresh cancel token (no client-side cancel propagation needed for
    // buffered ops; the OutputStream wraps the result immediately).
    let cancel = tokio_util::sync::CancellationToken::new();
    match service.generate(req, cancel).await {
        Ok(resp) => to_output(resp),
        Err(e) => {
            let (code, msg) = image_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("image.generate: {msg}")))
        }
    }
}

async fn list_models(service: &dyn ImageService) -> OutputStream {
    match service.list_models().await {
        Ok(models) => to_output(models),
        Err(e) => {
            let (code, msg) = image_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("image.list_models: {msg}")))
        }
    }
}

async fn status(service: &dyn ImageService, body: &[u8]) -> OutputStream {
    let req = decode_or_err!(body, wire::StatusRequest, "image.status");
    match service.status(&req.backend_id, &req.model_id).await {
        Ok(s) => to_output(s),
        Err(e) => {
            let (code, msg) = image_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("image.status: {msg}")))
        }
    }
}

async fn unload_model(service: &dyn ImageService, body: &[u8]) -> OutputStream {
    let req = decode_or_err!(body, wire::UnloadModelRequest, "image.unload_model");
    match service.unload_model(&req.backend_id, &req.model_id).await {
        Ok(()) => OutputStream::respond(vec![]),
        Err(e) => {
            let (code, msg) = image_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("image.unload_model: {msg}")))
        }
    }
}

// ---- Streaming ops ----

fn load_model(service: &Arc<dyn ImageService>, body: &[u8]) -> OutputStream {
    let req = decode_or_err!(body, wire::LoadModelRequest, "image.load_model");

    // The producer closure must be `'static`; clone the `Arc` into it.
    let service = Arc::clone(service);
    OutputStream::from_producer(move |sink, cancel| async move {
        let mut stream = service.load_model(&req.backend_id, &req.model_id, cancel);
        while let Some(item) = stream.next().await {
            let progress = match item {
                Ok(p) => p,
                Err(e) => {
                    let (code, msg) = image_error_to_block_error(e);
                    let _ = sink
                        .error(WaferError::new(code, format!("image.load_model: {msg}")))
                        .await;
                    return;
                }
            };
            let bytes = match codec::encode(&progress) {
                Ok(b) => b,
                Err(e) => {
                    let _ = sink
                        .error(WaferError::new(
                            ErrorCode::Internal,
                            format!("encoding load progress: {}", e.message),
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
