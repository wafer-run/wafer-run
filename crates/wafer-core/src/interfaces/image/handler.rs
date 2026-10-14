//! Shared message handler for image blocks.
//!
//! Decodes `msg.kind`, authorizes the caller for the op (see
//! [`handle_message`]), and translates the `ImageService` result onto an
//! `OutputStream`. Buffered ops (`generate`, `list_models`,
//! `status`, `unload_model`) produce a single `respond(body)`. The streaming
//! op (`load_model`) produces a `from_producer` stream: each service chunk is
//! codec-encoded (MessagePack) and emitted as its own `Chunk` event, and
//! cancellation from the consumer is forwarded straight through to the
//! service's cancel token. The relay does not rely on the service honouring
//! it: it races each read of the service's stream against the same token and
//! drops the stream when the consumer is gone
//! ([`handler_util::next_unless_cancelled`](crate::interfaces::handler_util::next_unless_cancelled)).
//!
//! The service trait operates directly on the `wafer_block::wire::image`
//! types (re-exported from `super::service`), so request/response values pass
//! straight through — only `ImageError` needs mapping onto wire error codes.

use std::sync::Arc;

use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    types::ResourceType,
    wire::image as wire,
    wrap::op_resource,
    *,
};

use super::service::{ImageError, ImageService};
use crate::interfaces::handler_util::{
    decode_and_authorize_model, next_unless_cancelled, to_output,
};

/// Decode an `image.*` request that names a model and authorize the caller
/// for `access` to that model in `block`'s namespace.
fn decode_for_model<T: serde::de::DeserializeOwned>(
    ctx: &dyn Context,
    block: &str,
    body: &[u8],
    op_name: &str,
    access: ResourceAccess,
    model: impl FnOnce(&T) -> (&str, &str),
) -> Result<T, OutputStream> {
    decode_and_authorize_model(
        ctx,
        block,
        body,
        op_name,
        ResourceType::Image,
        access,
        model,
    )
}

/// The caller-facing message for [`ImageError::Network`]. The provider's
/// own transport text stays in the server log.
pub const PROVIDER_UNREACHABLE: &str = "the image provider could not be reached";

/// Map a service-level `ImageError` onto a wire `ErrorCode` + message.
///
/// Both [`ImageError::Network`] and [`ImageError::EngineUnavailable`] map to
/// `Unavailable`, so the message is what tells them apart, and only the
/// second carries text written for the caller. A network failure's text is
/// the provider client's own (it can name hosts, addresses and URLs), so it
/// is logged and the caller gets the fixed [`PROVIDER_UNREACHABLE`]. An
/// edge that passes `Unavailable` through therefore never shows a caller
/// provider text.
fn image_error_to_block_error(e: ImageError) -> (ErrorCode, String) {
    match e {
        ImageError::NotSupported => (ErrorCode::Unimplemented, "not supported".to_string()),
        ImageError::InvalidRequest(msg) => (ErrorCode::InvalidArgument, msg),
        ImageError::BackendError(msg) => (ErrorCode::Internal, msg),
        ImageError::ModelNotFound(msg) => (ErrorCode::NotFound, msg),
        ImageError::Network(msg) => {
            tracing::warn!(error = %msg, "image provider unreachable");
            (ErrorCode::Unavailable, PROVIDER_UNREACHABLE.to_string())
        }
        // The reason is written for the caller (see the variant): returned as is.
        ImageError::EngineUnavailable(msg) => (ErrorCode::Unavailable, msg),
        ImageError::Cancelled => (ErrorCode::Cancelled, "cancelled".to_string()),
    }
}

// ---------- Entry point ----------

/// Dispatch an `image.*` message to the appropriate handler on `service` and
/// return the resulting output stream. Unknown ops yield an `INVALID_ARGUMENT`
/// error stream.
///
/// `block` is the registered name of the block serving the call, and `ctx`
/// its context for this call. Every op authorizes `ctx.caller_id()` through
/// `ctx.check_resource_access` against a [`ResourceType::Image`] resource in
/// `block`'s namespace before the service is touched: `generate` and
/// `status` read the model's
/// [`model_resource`](wafer_block::wrap::model_resource), `load_model` and
/// `unload_model` write it (they change what every other caller finds
/// loaded), and `list_models` reads [`op_resource`]. The serving block and
/// the admin block are admitted; any other caller needs a grant the serving
/// block declares (see [`ImageService::grants`]).
///
/// `service` is borrowed; the streaming op (`load_model`) clones the `Arc`
/// internally because its producer closure must be `'static`. Buffered ops
/// just borrow it.
pub async fn handle_message(
    service: &Arc<dyn ImageService>,
    ctx: &dyn Context,
    block: &str,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::IMAGE_GENERATE => generate(service.as_ref(), ctx, block, body).await,
        ServiceOp::IMAGE_LIST_MODELS => list_models(service.as_ref(), ctx, block).await,
        ServiceOp::IMAGE_STATUS => status(service.as_ref(), ctx, block, body).await,
        ServiceOp::IMAGE_LOAD_MODEL => load_model(service, ctx, block, body),
        ServiceOp::IMAGE_UNLOAD_MODEL => unload_model(service.as_ref(), ctx, block, body).await,
        other => OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("unknown image operation: {other}"),
        )),
    }
}

// ---- Buffered ops ----

async fn generate(
    service: &dyn ImageService,
    ctx: &dyn Context,
    block: &str,
    body: &[u8],
) -> OutputStream {
    let req = match decode_for_model(
        ctx,
        block,
        body,
        "image.generate",
        ResourceAccess::Read,
        |r: &wire::ImageRequest| (&r.backend_id, &r.model),
    ) {
        Ok(r) => r,
        Err(out) => return out,
    };
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

async fn list_models(service: &dyn ImageService, ctx: &dyn Context, block: &str) -> OutputStream {
    let resource = op_resource(block, ServiceOp::IMAGE_LIST_MODELS);
    if let Err(e) = ctx.check_resource_access(&resource, ResourceType::Image, ResourceAccess::Read)
    {
        return OutputStream::error(e);
    }
    match service.list_models().await {
        Ok(models) => to_output(models),
        Err(e) => {
            let (code, msg) = image_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("image.list_models: {msg}")))
        }
    }
}

async fn status(
    service: &dyn ImageService,
    ctx: &dyn Context,
    block: &str,
    body: &[u8],
) -> OutputStream {
    let req = match decode_for_model(
        ctx,
        block,
        body,
        "image.status",
        ResourceAccess::Read,
        |r: &wire::StatusRequest| (&r.backend_id, &r.model_id),
    ) {
        Ok(r) => r,
        Err(out) => return out,
    };
    match service.status(&req.backend_id, &req.model_id).await {
        Ok(s) => to_output(s),
        Err(e) => {
            let (code, msg) = image_error_to_block_error(e);
            OutputStream::error(WaferError::new(code, format!("image.status: {msg}")))
        }
    }
}

async fn unload_model(
    service: &dyn ImageService,
    ctx: &dyn Context,
    block: &str,
    body: &[u8],
) -> OutputStream {
    let req = match decode_for_model(
        ctx,
        block,
        body,
        "image.unload_model",
        ResourceAccess::Write,
        |r: &wire::UnloadModelRequest| (&r.backend_id, &r.model_id),
    ) {
        Ok(r) => r,
        Err(out) => return out,
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

fn load_model(
    service: &Arc<dyn ImageService>,
    ctx: &dyn Context,
    block: &str,
    body: &[u8],
) -> OutputStream {
    let req = match decode_for_model(
        ctx,
        block,
        body,
        "image.load_model",
        ResourceAccess::Write,
        |r: &wire::LoadModelRequest| (&r.backend_id, &r.model_id),
    ) {
        Ok(r) => r,
        Err(out) => return out,
    };

    // The producer closure must be `'static`; clone the `Arc` into it.
    let service = Arc::clone(service);
    OutputStream::from_producer(move |sink, cancel| async move {
        let mut stream = service.load_model(&req.backend_id, &req.model_id, cancel.clone());
        while let Some(item) = next_unless_cancelled(&cancel, &mut stream).await {
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
        let _ = sink.complete(vec![]).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A provider that cannot be reached is unavailable (a 503 at the HTTP
    /// edge), not an internal fault, and the provider client's text (which
    /// names hosts) is not what the caller reads.
    #[test]
    fn a_network_error_is_unavailable_and_scrubbed() {
        let (code, msg) = image_error_to_block_error(ImageError::Network(
            "error sending request for url (http://10.0.0.5:7860/sdapi/v1/txt2img)".into(),
        ));
        assert_eq!(code, ErrorCode::Unavailable);
        assert_eq!(msg, PROVIDER_UNREACHABLE);
    }

    /// An engine that cannot take the request is unavailable (a 503 at the
    /// HTTP edge), and the caller is told why, word for word.
    #[test]
    fn an_unavailable_engine_says_why() {
        let (code, msg) = image_error_to_block_error(ImageError::EngineUnavailable(
            "no open page has the model 'm' loaded — load it first".into(),
        ));
        assert_eq!(code, ErrorCode::Unavailable);
        assert_eq!(msg, "no open page has the model 'm' loaded — load it first");
    }

    /// A load relay whose consumer is gone lets go of the service's stream
    /// even when the service ignores its token and never yields; see the
    /// llm handler's test of the same name.
    #[tokio::test]
    async fn a_dropped_output_releases_a_silent_service_stream() {
        use std::time::Duration;

        use crate::test_support::{admit_all_context, Silent};

        struct Stuck(std::sync::Mutex<Option<Silent<Result<wire::LoadProgress, ImageError>>>>);

        #[wafer_block::wafer_async_trait]
        impl ImageService for Stuck {
            async fn generate(
                &self,
                _req: wire::ImageRequest,
                _cancel: tokio_util::sync::CancellationToken,
            ) -> Result<wire::ImageResponse, ImageError> {
                Err(ImageError::NotSupported)
            }
            async fn list_models(&self) -> Result<Vec<wire::ModelInfo>, ImageError> {
                Ok(Vec::new())
            }
            async fn status(&self, _: &str, _: &str) -> Result<wire::ModelStatus, ImageError> {
                Ok(wire::ModelStatus::ready())
            }
            fn load_model(
                &self,
                _backend_id: &str,
                _model_id: &str,
                _cancel: tokio_util::sync::CancellationToken,
            ) -> futures::stream::BoxStream<'static, Result<wire::LoadProgress, ImageError>>
            {
                Box::pin(self.0.lock().unwrap().take().expect("one load"))
            }
        }

        let (load, load_dropped) = Silent::new();
        let service: Arc<dyn ImageService> = Arc::new(Stuck(std::sync::Mutex::new(Some(load))));
        let ctx = admit_all_context();
        let out = handle_message(
            &service,
            ctx.as_ref(),
            "wafer-run/image",
            &Message::new(ServiceOp::IMAGE_LOAD_MODEL),
            &codec::encode(&wire::LoadModelRequest {
                backend_id: "b".into(),
                model_id: "m".into(),
            })
            .expect("encode"),
        )
        .await;

        drop(out);

        tokio::time::timeout(Duration::from_secs(5), load_dropped)
            .await
            .expect("the load relay lets go of the service's stream")
            .expect("the drop is reported");
    }
}
