//! Shared message handler for LLM blocks.
//!
//! Decodes `msg.kind` into calls on a `LlmService` impl and translates the
//! result onto an `OutputStream`. Buffered ops (`list_models`, `status`,
//! `unload_model`) produce a single `respond(body)`. Streaming ops (`chat`,
//! `load_model`) produce a `from_producer` stream: each service chunk is
//! codec-encoded (MessagePack) and emitted as its own `Chunk` event, and
//! cancellation from the consumer is forwarded straight through to the
//! service's cancel token.
//!
//! The service trait operates directly on the `wafer_block::wire::llm` types
//! (re-exported from `super::service`), so request/response values pass
//! straight through — only `LlmError` needs mapping onto wire error codes.

use std::sync::Arc;

use futures::StreamExt;
use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    wire::llm as wire,
    *,
};

use super::service::{LlmError, LlmService};
use crate::interfaces::handler_util::{decode_or_err, to_output};

/// Map a service-level `LlmError` onto a wire `ErrorCode` + message. Mirrors
/// `image::handler::image_error_to_block_error` so callers surface the right
/// status instead of collapsing everything to `INTERNAL`.
fn llm_error_to_block_error(e: LlmError) -> (ErrorCode, String) {
    match e {
        LlmError::NotSupported => (ErrorCode::Unimplemented, "not supported".to_string()),
        LlmError::InvalidRequest(msg) => (ErrorCode::InvalidArgument, msg),
        LlmError::BackendError(msg) => (ErrorCode::Internal, msg),
        LlmError::ModelNotFound(msg) => (ErrorCode::NotFound, msg),
        LlmError::RateLimited => (ErrorCode::Unavailable, "rate limited".to_string()),
        LlmError::Unauthorized => (ErrorCode::Unauthenticated, "unauthorized".to_string()),
        LlmError::Network(msg) => (ErrorCode::Internal, format!("network: {msg}")),
        LlmError::Cancelled => (ErrorCode::Cancelled, "cancelled".to_string()),
    }
}

// ---------- Entry point ----------

/// Dispatch an `llm.*` message to the appropriate handler on `service` and
/// return the resulting output stream. Unknown ops yield an `INVALID_ARGUMENT`
/// error stream.
///
/// `service` is borrowed; the streaming ops (`chat`, `load_model`) clone the
/// `Arc` internally because their producer closures must be `'static`. Buffered
/// ops just borrow it.
pub async fn handle_message(
    service: &Arc<dyn LlmService>,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::LLM_CHAT => chat(service, body),
        ServiceOp::LLM_LIST_MODELS => list_models(service.as_ref()).await,
        ServiceOp::LLM_STATUS => status(service.as_ref(), body).await,
        ServiceOp::LLM_LOAD_MODEL => load_model(service, body),
        ServiceOp::LLM_UNLOAD_MODEL => unload_model(service.as_ref(), body).await,
        other => OutputStream::error(WaferError::new(
            ErrorCode::InvalidArgument,
            format!("unknown llm operation: {other}"),
        )),
    }
}

// ---- Streaming ops ----

fn chat(service: &Arc<dyn LlmService>, body: &[u8]) -> OutputStream {
    // Decode up front — failures become an error stream rather than a malformed
    // chunk halfway through.
    let req = decode_or_err!(body, wire::ChatRequest, "llm.chat");

    // The producer closure must be `'static`; clone the `Arc` into it.
    let service = Arc::clone(service);
    OutputStream::from_producer(move |sink, cancel| async move {
        let mut stream = service.chat_stream(req, cancel).await;
        while let Some(item) = stream.next().await {
            // Each frame is a `wire::ChatChunk` directly. Service-level
            // `LlmError` is surfaced as a terminal stream error rather than
            // a Result-wrapped chunk, matching the SDK's `next_chunk` decode
            // (which treats every frame as a `ChatChunk`).
            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    let (code, msg) = llm_error_to_block_error(e);
                    let _ = sink
                        .error(WaferError::new(code, format!("llm.chat: {msg}")))
                        .await;
                    return;
                }
            };
            let bytes = match codec::encode(&chunk) {
                Ok(b) => b,
                Err(e) => {
                    let _ = sink
                        .error(WaferError::new(
                            ErrorCode::Internal,
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

fn load_model(service: &Arc<dyn LlmService>, body: &[u8]) -> OutputStream {
    let req = decode_or_err!(body, wire::LoadModelRequest, "llm.load_model");

    // The producer closure must be `'static`; clone the `Arc` into it.
    let service = Arc::clone(service);
    OutputStream::from_producer(move |sink, cancel| async move {
        let mut stream = service.load_model(&req.backend_id, &req.model_id, cancel);
        while let Some(item) = stream.next().await {
            let progress = match item {
                Ok(p) => p,
                Err(e) => {
                    let (code, msg) = llm_error_to_block_error(e);
                    let _ = sink
                        .error(WaferError::new(code, format!("llm.load_model: {msg}")))
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
                return;
            }
        }
    })
}

// ---- Buffered ops ----

async fn list_models(service: &dyn LlmService) -> OutputStream {
    match service.list_models().await {
        Ok(models) => to_output(models),
        Err(e) => {
            let (code, msg) = llm_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("list_models: {msg}")))
        }
    }
}

async fn status(service: &dyn LlmService, body: &[u8]) -> OutputStream {
    let req = decode_or_err!(body, wire::StatusRequest, "llm.status");
    match service.status(&req.backend_id, &req.model_id).await {
        Ok(s) => to_output(s),
        Err(e) => {
            let (code, msg) = llm_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("status: {msg}")))
        }
    }
}

async fn unload_model(service: &dyn LlmService, body: &[u8]) -> OutputStream {
    let req = decode_or_err!(body, wire::UnloadModelRequest, "llm.unload_model");
    match service.unload_model(&req.backend_id, &req.model_id).await {
        Ok(()) => OutputStream::respond(vec![]),
        Err(e) => {
            let (code, msg) = llm_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("unload_model: {msg}")))
        }
    }
}
