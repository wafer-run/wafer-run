//! Shared message handler for LLM blocks.
//!
//! Decodes `msg.kind` into calls on a `LlmService` impl and translates the
//! result onto an `OutputStream`. Buffered ops (`list_models`, `status`,
//! `unload_model`) produce a single `respond(body)`. Streaming ops (`chat`,
//! `load_model`) produce a `from_producer` stream: each service chunk is
//! JSON-encoded and emitted as a `Chunk` event, and cancellation from the
//! consumer is forwarded straight through to the service's cancel token.

use std::sync::Arc;

use futures::StreamExt;
use serde::Deserialize;
use wafer_block::{
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    *,
};

use super::service::LlmService;
use crate::interfaces::handler_util::{decode_or_err, to_output};

#[derive(Deserialize)]
struct StatusRequest {
    backend_id: String,
    model_id: String,
}

#[derive(Deserialize)]
struct LoadModelRequest {
    backend_id: String,
    model_id: String,
}

#[derive(Deserialize)]
struct UnloadModelRequest {
    backend_id: String,
    model_id: String,
}

pub async fn handle_message(
    service: Arc<dyn LlmService>,
    msg: &Message,
    body: Vec<u8>,
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::LLM_CHAT => chat(service, body),
        ServiceOp::LLM_LIST_MODELS => list_models(service).await,
        ServiceOp::LLM_STATUS => status(service, &body).await,
        ServiceOp::LLM_LOAD_MODEL => load_model(service, &body),
        ServiceOp::LLM_UNLOAD_MODEL => unload_model(service, &body).await,
        other => OutputStream::error(WaferError::new(
            ErrorCode::INVALID_ARGUMENT,
            format!("unknown llm operation: {other}"),
        )),
    }
}

// ---- Streaming ops ----

fn chat(service: Arc<dyn LlmService>, body: Vec<u8>) -> OutputStream {
    // Decode up front — failures become an error stream rather than a malformed
    // chunk halfway through.
    let req = match serde_json::from_slice::<super::service::ChatRequest>(&body) {
        Ok(r) => r,
        Err(e) => {
            return OutputStream::error(WaferError::new(
                ErrorCode::INVALID_ARGUMENT,
                format!("invalid llm.chat request: {e}"),
            ));
        }
    };

    OutputStream::from_producer(move |sink, cancel| async move {
        let mut stream = service.chat_stream(req, cancel).await;
        while let Some(item) = stream.next().await {
            let bytes = match serde_json::to_vec(&item) {
                Ok(b) => b,
                Err(e) => {
                    let _ = sink
                        .error(WaferError::new(
                            ErrorCode::INTERNAL,
                            format!("serialize chat chunk: {e}"),
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

fn load_model(service: Arc<dyn LlmService>, body: &[u8]) -> OutputStream {
    let req = match serde_json::from_slice::<LoadModelRequest>(body) {
        Ok(r) => r,
        Err(e) => {
            return OutputStream::error(WaferError::new(
                ErrorCode::INVALID_ARGUMENT,
                format!("invalid llm.load_model request: {e}"),
            ));
        }
    };

    OutputStream::from_producer(move |sink, cancel| async move {
        let mut stream = service.load_model(&req.backend_id, &req.model_id, cancel);
        while let Some(item) = stream.next().await {
            let bytes = match serde_json::to_vec(&item) {
                Ok(b) => b,
                Err(e) => {
                    let _ = sink
                        .error(WaferError::new(
                            ErrorCode::INTERNAL,
                            format!("serialize load progress: {e}"),
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

async fn list_models(service: Arc<dyn LlmService>) -> OutputStream {
    match service.list_models().await {
        Ok(models) => to_output(&models),
        Err(e) => OutputStream::error(WaferError::new(
            ErrorCode::INTERNAL,
            format!("list_models: {e}"),
        )),
    }
}

async fn status(service: Arc<dyn LlmService>, body: &[u8]) -> OutputStream {
    let req = decode_or_err!(body, StatusRequest, "llm.status");
    match service.status(&req.backend_id, &req.model_id).await {
        Ok(s) => to_output(&s),
        Err(e) => OutputStream::error(WaferError::new(ErrorCode::INTERNAL, format!("status: {e}"))),
    }
}

async fn unload_model(service: Arc<dyn LlmService>, body: &[u8]) -> OutputStream {
    let req = decode_or_err!(body, UnloadModelRequest, "llm.unload_model");
    match service.unload_model(&req.backend_id, &req.model_id).await {
        Ok(()) => OutputStream::respond(vec![]),
        Err(e) => OutputStream::error(WaferError::new(
            ErrorCode::INTERNAL,
            format!("unload_model: {e}"),
        )),
    }
}
