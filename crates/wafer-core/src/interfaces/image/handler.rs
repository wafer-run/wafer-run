//! Shared message handler for image blocks.
//!
//! Decodes `msg.kind` into calls on an `ImageService` impl and translates the
//! result onto an `OutputStream`. Buffered ops (`generate`, `list_models`,
//! `status`, `unload_model`) produce a single `respond(body)`. The streaming
//! op (`load_model`) produces a `from_producer` stream: each service chunk is
//! codec-encoded (MessagePack) and emitted as its own `Chunk` event, and
//! cancellation from the consumer is forwarded straight through to the
//! service's cancel token.

use std::sync::Arc;

use futures::StreamExt;
use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    wire::image as wire,
    *,
};

use super::service::{self, ImageService};

// ---------- Wire <-> service conversions ----------
//
// Field-identical types — conversion is mechanical. We keep them in the
// handler rather than implementing `From` on the service crate because the
// service trait is intentionally agnostic of the wire representation.

fn wire_request_to_service(req: wire::ImageRequest) -> service::ImageRequest {
    service::ImageRequest {
        backend_id: req.backend_id,
        model: req.model,
        prompt: req.prompt,
        params: wire_params_to_service(req.params),
        extra: req.extra,
    }
}

fn wire_params_to_service(p: wire::ImageParams) -> service::ImageParams {
    service::ImageParams {
        negative_prompt: p.negative_prompt,
        width: p.width,
        height: p.height,
        steps: p.steps,
        guidance_scale: p.guidance_scale,
        seed: p.seed,
    }
}

fn service_response_to_wire(resp: service::ImageResponse) -> wire::ImageResponse {
    wire::ImageResponse {
        images: resp
            .images
            .into_iter()
            .map(|img| wire::GeneratedImage {
                bytes: img.bytes,
                mime_type: img.mime_type,
            })
            .collect(),
    }
}

fn service_model_info_to_wire(info: service::ModelInfo) -> wire::ModelInfo {
    wire::ModelInfo {
        backend_id: info.backend_id,
        model_id: info.model_id,
        display_name: info.display_name,
        capabilities: wire::ModelCapabilities {
            max_width: info.capabilities.max_width,
            max_height: info.capabilities.max_height,
            supports_negative_prompt: info.capabilities.supports_negative_prompt,
            max_steps: info.capabilities.max_steps,
        },
    }
}

fn service_status_to_wire(s: service::ModelStatus) -> wire::ModelStatus {
    wire::ModelStatus {
        state: service_state_to_wire(s.state),
        progress: s.progress,
    }
}

fn service_state_to_wire(s: service::ModelState) -> wire::ModelState {
    match s {
        service::ModelState::Ready => wire::ModelState::Ready,
        service::ModelState::Loading => wire::ModelState::Loading,
        service::ModelState::Unloaded => wire::ModelState::Unloaded,
        service::ModelState::Error { message } => wire::ModelState::Error { message },
    }
}

fn service_progress_to_wire(p: service::LoadProgress) -> wire::LoadProgress {
    wire::LoadProgress {
        stage: p.stage,
        bytes_downloaded: p.bytes_downloaded,
        bytes_total: p.bytes_total,
    }
}

fn image_error_to_block_error(e: service::ImageError) -> (ErrorCode, String) {
    match e {
        service::ImageError::NotSupported => {
            (ErrorCode::UNIMPLEMENTED, "not supported".to_string())
        }
        service::ImageError::InvalidRequest(msg) => (ErrorCode::INVALID_ARGUMENT, msg),
        service::ImageError::BackendError(msg) => (ErrorCode::INTERNAL, msg),
        service::ImageError::ModelNotFound(msg) => (ErrorCode::NOT_FOUND, msg),
        service::ImageError::Network(msg) => (ErrorCode::INTERNAL, format!("network: {msg}")),
        service::ImageError::Cancelled => (ErrorCode::CANCELLED, "cancelled".to_string()),
    }
}

// ---------- Entry point ----------

/// Dispatch an `image.*` message to the appropriate handler on `service` and
/// return the resulting output stream. Unknown ops yield an `INVALID_ARGUMENT`
/// error stream.
pub async fn handle_message(
    service: Arc<dyn ImageService>,
    msg: &Message,
    body: Vec<u8>,
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::IMAGE_GENERATE => generate(service, body).await,
        ServiceOp::IMAGE_LIST_MODELS => list_models(service).await,
        ServiceOp::IMAGE_STATUS => status(service, &body).await,
        ServiceOp::IMAGE_LOAD_MODEL => load_model(service, &body),
        ServiceOp::IMAGE_UNLOAD_MODEL => unload_model(service, &body).await,
        other => OutputStream::error(WaferError::new(
            ErrorCode::INVALID_ARGUMENT,
            format!("unknown image operation: {other}"),
        )),
    }
}

// ---- Buffered ops ----

async fn generate(service: Arc<dyn ImageService>, body: Vec<u8>) -> OutputStream {
    let wire_req: wire::ImageRequest = match codec::decode(&body) {
        Ok(r) => r,
        Err(e) => {
            return OutputStream::error(WaferError::new(
                ErrorCode::INVALID_ARGUMENT,
                format!("invalid image.generate request: {}", e.message),
            ));
        }
    };
    let req = wire_request_to_service(wire_req);
    // `generate` is not streaming — the whole response arrives at once.
    // Use a fresh cancel token (no client-side cancel propagation needed for
    // buffered ops; the OutputStream wraps the result immediately).
    let cancel = tokio_util::sync::CancellationToken::new();
    match service.generate(req, cancel).await {
        Ok(resp) => {
            let wire_resp = service_response_to_wire(resp);
            match codec::encode(&wire_resp) {
                Ok(bytes) => OutputStream::respond(bytes),
                Err(e) => OutputStream::error(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("encoding image.generate response: {}", e.message),
                )),
            }
        }
        Err(e) => {
            let (code, msg) = image_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("image.generate: {msg}")))
        }
    }
}

async fn list_models(service: Arc<dyn ImageService>) -> OutputStream {
    match service.list_models().await {
        Ok(models) => {
            let wire_models: Vec<wire::ModelInfo> =
                models.into_iter().map(service_model_info_to_wire).collect();
            match codec::encode(&wire_models) {
                Ok(bytes) => OutputStream::respond(bytes),
                Err(e) => OutputStream::error(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("encoding image.list_models response: {}", e.message),
                )),
            }
        }
        Err(e) => {
            let (code, msg) = image_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("image.list_models: {msg}")))
        }
    }
}

async fn status(service: Arc<dyn ImageService>, body: &[u8]) -> OutputStream {
    let req: wire::StatusRequest = match codec::decode(body) {
        Ok(r) => r,
        Err(e) => {
            return OutputStream::error(WaferError::new(
                ErrorCode::INVALID_ARGUMENT,
                format!("invalid image.status request: {}", e.message),
            ));
        }
    };
    match service.status(&req.backend_id, &req.model_id).await {
        Ok(s) => {
            let wire_status = service_status_to_wire(s);
            match codec::encode(&wire_status) {
                Ok(bytes) => OutputStream::respond(bytes),
                Err(e) => OutputStream::error(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("encoding image.status response: {}", e.message),
                )),
            }
        }
        Err(e) => {
            let (code, msg) = image_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("image.status: {msg}")))
        }
    }
}

async fn unload_model(service: Arc<dyn ImageService>, body: &[u8]) -> OutputStream {
    let req: wire::UnloadModelRequest = match codec::decode(body) {
        Ok(r) => r,
        Err(e) => {
            return OutputStream::error(WaferError::new(
                ErrorCode::INVALID_ARGUMENT,
                format!("invalid image.unload_model request: {}", e.message),
            ));
        }
    };
    match service.unload_model(&req.backend_id, &req.model_id).await {
        Ok(()) => OutputStream::respond(vec![]),
        Err(e) => {
            let (code, msg) = image_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("image.unload_model: {msg}")))
        }
    }
}

// ---- Streaming ops ----

fn load_model(service: Arc<dyn ImageService>, body: &[u8]) -> OutputStream {
    let req: wire::LoadModelRequest = match codec::decode(body) {
        Ok(r) => r,
        Err(e) => {
            return OutputStream::error(WaferError::new(
                ErrorCode::INVALID_ARGUMENT,
                format!("invalid image.load_model request: {}", e.message),
            ));
        }
    };

    OutputStream::from_producer(move |sink, cancel| async move {
        let mut stream = service.load_model(&req.backend_id, &req.model_id, cancel);
        while let Some(item) = stream.next().await {
            let progress = match item {
                Ok(p) => service_progress_to_wire(p),
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
                            ErrorCode::INTERNAL,
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
