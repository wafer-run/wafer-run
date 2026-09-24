//! Shared message handler for LLM blocks.
//!
//! Decodes `msg.kind`, authorizes the caller for the op (see
//! [`handle_message`]), and translates the `LlmService` result onto an
//! `OutputStream`. Buffered ops (`list_models`, `status`,
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
    types::ResourceType,
    wire::llm as wire,
    wrap::op_resource,
    *,
};

use super::service::{LlmError, LlmService};
use crate::interfaces::handler_util::{decode_and_authorize_model, to_output};

/// Decode an `llm.*` request that names a model and authorize the caller for
/// `access` to that model in `block`'s namespace.
fn decode_for_model<T: serde::de::DeserializeOwned>(
    ctx: &dyn Context,
    block: &str,
    body: &[u8],
    op_name: &str,
    access: ResourceAccess,
    model: impl FnOnce(&T) -> (&str, &str),
) -> Result<T, OutputStream> {
    decode_and_authorize_model(ctx, block, body, op_name, ResourceType::Llm, access, model)
}

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
        LlmError::Network(msg) => (ErrorCode::Unavailable, format!("network: {msg}")),
        LlmError::Cancelled => (ErrorCode::Cancelled, "cancelled".to_string()),
    }
}

// ---------- Entry point ----------

/// Dispatch an `llm.*` message to the appropriate handler on `service` and
/// return the resulting output stream. Unknown ops yield an `INVALID_ARGUMENT`
/// error stream.
///
/// `block` is the registered name of the block serving the call, and `ctx`
/// its context for this call. Every op authorizes `ctx.caller_id()` through
/// `ctx.check_resource_access` against a [`ResourceType::Llm`] resource in
/// `block`'s namespace before the service is touched: `chat` and `status`
/// read the model's [`model_resource`](wafer_block::wrap::model_resource), `load_model` and `unload_model`
/// write it (they change what every other caller finds loaded), and
/// `list_models` reads [`op_resource`]. The serving block and the admin
/// block are admitted; any other caller needs a grant the serving block
/// declares (see [`LlmService::grants`]).
///
/// `service` is borrowed; the streaming ops (`chat`, `load_model`) clone the
/// `Arc` internally because their producer closures must be `'static`. Buffered
/// ops just borrow it.
pub async fn handle_message(
    service: &Arc<dyn LlmService>,
    ctx: &dyn Context,
    block: &str,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::LLM_CHAT => chat(service, ctx, block, body),
        ServiceOp::LLM_LIST_MODELS => list_models(service.as_ref(), ctx, block).await,
        ServiceOp::LLM_STATUS => status(service.as_ref(), ctx, block, body).await,
        ServiceOp::LLM_LOAD_MODEL => load_model(service, ctx, block, body),
        ServiceOp::LLM_UNLOAD_MODEL => unload_model(service.as_ref(), ctx, block, body).await,
        other => OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("unknown llm operation: {other}"),
        )),
    }
}

// ---- Streaming ops ----

fn chat(
    service: &Arc<dyn LlmService>,
    ctx: &dyn Context,
    block: &str,
    body: &[u8],
) -> OutputStream {
    // Decode and authorize up front — failures become an error stream rather
    // than a malformed chunk halfway through.
    let req = match decode_for_model(
        ctx,
        block,
        body,
        "llm.chat",
        ResourceAccess::Read,
        |r: &wire::ChatRequest| (&r.backend_id, &r.model),
    ) {
        Ok(r) => r,
        Err(out) => return out,
    };

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

fn load_model(
    service: &Arc<dyn LlmService>,
    ctx: &dyn Context,
    block: &str,
    body: &[u8],
) -> OutputStream {
    let req = match decode_for_model(
        ctx,
        block,
        body,
        "llm.load_model",
        ResourceAccess::Write,
        |r: &wire::LoadModelRequest| (&r.backend_id, &r.model_id),
    ) {
        Ok(r) => r,
        Err(out) => return out,
    };

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

async fn list_models(service: &dyn LlmService, ctx: &dyn Context, block: &str) -> OutputStream {
    let resource = op_resource(block, ServiceOp::LLM_LIST_MODELS);
    if let Err(e) = ctx.check_resource_access(&resource, ResourceType::Llm, ResourceAccess::Read) {
        return OutputStream::error(e);
    }
    match service.list_models().await {
        Ok(models) => to_output(models),
        Err(e) => {
            let (code, msg) = llm_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("list_models: {msg}")))
        }
    }
}

async fn status(
    service: &dyn LlmService,
    ctx: &dyn Context,
    block: &str,
    body: &[u8],
) -> OutputStream {
    let req = match decode_for_model(
        ctx,
        block,
        body,
        "llm.status",
        ResourceAccess::Read,
        |r: &wire::StatusRequest| (&r.backend_id, &r.model_id),
    ) {
        Ok(r) => r,
        Err(out) => return out,
    };
    match service.status(&req.backend_id, &req.model_id).await {
        Ok(s) => to_output(s),
        Err(e) => {
            let (code, msg) = llm_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("status: {msg}")))
        }
    }
}

async fn unload_model(
    service: &dyn LlmService,
    ctx: &dyn Context,
    block: &str,
    body: &[u8],
) -> OutputStream {
    let req = match decode_for_model(
        ctx,
        block,
        body,
        "llm.unload_model",
        ResourceAccess::Write,
        |r: &wire::UnloadModelRequest| (&r.backend_id, &r.model_id),
    ) {
        Ok(r) => r,
        Err(out) => return out,
    };
    match service.unload_model(&req.backend_id, &req.model_id).await {
        Ok(()) => OutputStream::respond(vec![]),
        Err(e) => {
            let (code, msg) = llm_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("unload_model: {msg}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A provider that cannot be reached is unavailable (a 503 at the HTTP
    /// edge), not an internal fault.
    #[test]
    fn a_network_error_is_unavailable() {
        let (code, msg) = llm_error_to_block_error(LlmError::Network("connection refused".into()));
        assert_eq!(code, ErrorCode::Unavailable);
        assert_eq!(msg, "network: connection refused");
    }
}
