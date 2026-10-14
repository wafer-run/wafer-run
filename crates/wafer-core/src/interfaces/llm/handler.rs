//! Shared message handler for LLM blocks.
//!
//! Decodes `msg.kind`, authorizes the caller for the op (see
//! [`handle_message`]), and translates the `LlmService` result onto an
//! `OutputStream`. Buffered ops (`list_models`, `status`,
//! `unload_model`) produce a single `respond(body)`. Streaming ops (`chat`,
//! `load_model`) produce a `from_producer` stream: each service chunk is
//! codec-encoded (MessagePack) and emitted as its own `Chunk` event, and
//! cancellation from the consumer is forwarded straight through to the
//! service's cancel token. The relay does not rely on the service honouring
//! it: it races each read of the service's stream against the same token and
//! drops the stream when the consumer is gone
//! ([`handler_util::next_unless_cancelled`](crate::interfaces::handler_util::next_unless_cancelled)).
//!
//! The service trait operates directly on the `wafer_block::wire::llm` types
//! (re-exported from `super::service`), so request/response values pass
//! straight through — only `LlmError` needs mapping onto wire error codes.

use std::sync::Arc;

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
use crate::interfaces::handler_util::{
    decode_and_authorize_model, next_unless_cancelled, to_output, Relayed,
};

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

/// The caller-facing message for [`LlmError::Network`]. The provider's own
/// transport text stays in the server log.
pub const PROVIDER_UNREACHABLE: &str = "the model provider could not be reached";

/// Map a service-level `LlmError` onto a wire `ErrorCode` + message. Mirrors
/// `image::handler::image_error_to_block_error` so callers surface the right
/// status instead of collapsing everything to `INTERNAL`.
///
/// Both [`LlmError::Network`] and [`LlmError::EngineUnavailable`] map to
/// `Unavailable`, so the message is what tells them apart, and only the
/// second carries text written for the caller. A network failure's text is
/// the provider client's own (it can name hosts, addresses and URLs), so it
/// is logged and the caller gets the fixed [`PROVIDER_UNREACHABLE`]. An
/// edge that passes `Unavailable` through therefore never shows a caller
/// provider text.
fn llm_error_to_block_error(e: LlmError) -> (ErrorCode, String) {
    match e {
        LlmError::NotSupported => (ErrorCode::Unimplemented, "not supported".to_string()),
        LlmError::InvalidRequest(msg) => (ErrorCode::InvalidArgument, msg),
        LlmError::BackendError(msg) => (ErrorCode::Internal, msg),
        LlmError::ModelNotFound(msg) => (ErrorCode::NotFound, msg),
        LlmError::RateLimited => (ErrorCode::Unavailable, "rate limited".to_string()),
        LlmError::Unauthorized => (ErrorCode::Unauthenticated, "unauthorized".to_string()),
        LlmError::Network(msg) => {
            tracing::warn!(error = %msg, "llm provider unreachable");
            (ErrorCode::Unavailable, PROVIDER_UNREACHABLE.to_string())
        }
        // The reason is written for the caller (see the variant): returned as is.
        LlmError::EngineUnavailable(msg) => (ErrorCode::Unavailable, msg),
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
        // `chat_stream` is async: a service may await (connect, queue) before
        // it has a stream to hand back. That await is raced too, so a service
        // that misses the token there does not hold this task either.
        let Some(mut stream) = cancel
            .run_until_cancelled(service.chat_stream(req, cancel.clone()))
            .await
        else {
            return;
        };
        loop {
            let item = match next_unless_cancelled(&cancel, &mut stream).await {
                Relayed::Item(item) => item,
                Relayed::Ended => break,
                // No terminal: see `Relayed::Cancelled`.
                Relayed::Cancelled => return,
            };
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
        let _ = sink.complete(vec![]).await;
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
        let mut stream = service.load_model(&req.backend_id, &req.model_id, cancel.clone());
        loop {
            let item = match next_unless_cancelled(&cancel, &mut stream).await {
                Relayed::Item(item) => item,
                Relayed::Ended => break,
                // No terminal: see `Relayed::Cancelled`.
                Relayed::Cancelled => return,
            };
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
        let _ = sink.complete(vec![]).await;
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
    use crate::test_support::Silent;

    /// A provider that cannot be reached is unavailable (a 503 at the HTTP
    /// edge), not an internal fault, and the provider client's text (which
    /// names hosts) is not what the caller reads.
    #[test]
    fn a_network_error_is_unavailable_and_scrubbed() {
        let (code, msg) = llm_error_to_block_error(LlmError::Network(
            "error sending request for url (http://10.0.0.5:11434/api/chat): connection refused"
                .into(),
        ));
        assert_eq!(code, ErrorCode::Unavailable);
        assert_eq!(msg, PROVIDER_UNREACHABLE);
    }

    /// An engine that cannot take the request is unavailable (a 503 at the
    /// HTTP edge), and the caller is told why, word for word.
    #[test]
    fn an_unavailable_engine_says_why() {
        let (code, msg) = llm_error_to_block_error(LlmError::EngineUnavailable(
            "no open page has the model 'm' loaded — load it first".into(),
        ));
        assert_eq!(code, ErrorCode::Unavailable);
        assert_eq!(msg, "no open page has the model 'm' loaded — load it first");
    }

    /// A service that never yields and ignores its token: its chat either
    /// hangs in `chat_stream` itself (the setup await) or hands back a stream
    /// that never yields; its load always hands back such a stream. Each
    /// reports when it is entered, and each `Silent` reports when it is
    /// dropped.
    struct Stuck {
        setup_hangs: bool,
        entered: std::sync::Mutex<Option<futures::channel::oneshot::Sender<()>>>,
        chat: std::sync::Mutex<Option<Silent<Result<wire::ChatChunk, LlmError>>>>,
        load: std::sync::Mutex<Option<Silent<Result<wire::LoadProgress, LlmError>>>>,
    }

    impl Stuck {
        /// The service, the receiver its chat's entry resolves, and the
        /// receivers the chat's and the load's `Silent` drops resolve.
        fn service(
            setup_hangs: bool,
        ) -> (
            Arc<dyn LlmService>,
            futures::channel::oneshot::Receiver<()>,
            futures::channel::oneshot::Receiver<()>,
            futures::channel::oneshot::Receiver<()>,
        ) {
            let (entered, chat_entered) = futures::channel::oneshot::channel();
            let (chat, chat_dropped) = Silent::new();
            let (load, load_dropped) = Silent::new();
            let service = Arc::new(Self {
                setup_hangs,
                entered: std::sync::Mutex::new(Some(entered)),
                chat: std::sync::Mutex::new(Some(chat)),
                load: std::sync::Mutex::new(Some(load)),
            });
            (service, chat_entered, chat_dropped, load_dropped)
        }
    }

    #[wafer_block::wafer_async_trait]
    impl LlmService for Stuck {
        async fn chat_stream(
            &self,
            _req: wire::ChatRequest,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> futures::stream::BoxStream<'static, Result<wire::ChatChunk, LlmError>> {
            let silent = self.chat.lock().unwrap().take().expect("one chat");
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            if self.setup_hangs {
                // Held across the await, so it is dropped with this future.
                let _held = silent;
                std::future::pending::<()>().await;
                unreachable!("a pending future never resolves");
            }
            Box::pin(silent)
        }
        async fn list_models(&self) -> Result<Vec<wire::ModelInfo>, LlmError> {
            Ok(Vec::new())
        }
        async fn status(&self, _: &str, _: &str) -> Result<wire::ModelStatus, LlmError> {
            Ok(wire::ModelStatus::ready())
        }
        fn load_model(
            &self,
            _backend_id: &str,
            _model_id: &str,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> futures::stream::BoxStream<'static, Result<wire::LoadProgress, LlmError>> {
            Box::pin(self.load.lock().unwrap().take().expect("one load"))
        }
    }

    fn chat_body() -> Vec<u8> {
        codec::encode(&wire::ChatRequest {
            backend_id: "b".into(),
            model: "m".into(),
            messages: vec![wire::ChatMessage::user("hi")],
            params: wire::ChatParams::default(),
            tools: Vec::new(),
            extra: serde_json::Value::Null,
        })
        .expect("encode")
    }

    fn load_body() -> Vec<u8> {
        codec::encode(&wire::LoadModelRequest {
            backend_id: "b".into(),
            model_id: "m".into(),
        })
        .expect("encode")
    }

    async fn run(service: &Arc<dyn LlmService>, op: &str, body: &[u8]) -> OutputStream {
        let ctx = crate::test_support::admit_all_context();
        handle_message(
            service,
            ctx.as_ref(),
            "wafer-run/llm",
            &Message::new(op),
            body,
        )
        .await
    }

    /// `what`, or a failure after five seconds. The bound only turns a hang
    /// into a failure.
    async fn within<F: std::future::Future>(what: &str, fut: F) -> F::Output {
        tokio::time::timeout(std::time::Duration::from_secs(5), fut)
            .await
            .unwrap_or_else(|_| panic!("{what}"))
    }

    /// A relay whose consumer is gone lets go of the service's stream even
    /// when the service ignores its cancellation token and never yields: the
    /// relay itself races its read against the token. It used to wait on the
    /// service's next item for good, holding the stream — and whatever the
    /// service keeps alive for it — until the service spoke.
    #[tokio::test]
    async fn a_dropped_output_releases_a_silent_service_stream() {
        let (service, chat_entered, chat_dropped, load_dropped) = Stuck::service(false);

        let chat_out = run(&service, ServiceOp::LLM_CHAT, &chat_body()).await;
        let load_out = run(&service, ServiceOp::LLM_LOAD_MODEL, &load_body()).await;
        within("the chat reaches the service", chat_entered)
            .await
            .expect("entered");

        drop(chat_out);
        drop(load_out);

        within(
            "the chat relay lets go of the service's stream",
            chat_dropped,
        )
        .await
        .expect("the drop is reported");
        within(
            "the load relay lets go of the service's stream",
            load_dropped,
        )
        .await
        .expect("the drop is reported");
    }

    /// The same for a service whose `chat_stream` never returns a stream: the
    /// relay's await on it is raced too, so a service stuck in its setup (a
    /// connect, a queue) that misses the token does not hold the relay.
    #[tokio::test]
    async fn a_dropped_output_releases_a_service_stuck_in_its_chat_setup() {
        let (service, chat_entered, chat_dropped, _load_dropped) = Stuck::service(true);

        let chat_out = run(&service, ServiceOp::LLM_CHAT, &chat_body()).await;
        within("the chat reaches the service", chat_entered)
            .await
            .expect("entered");

        drop(chat_out);

        within(
            "the chat relay lets go of the service's setup",
            chat_dropped,
        )
        .await
        .expect("the drop is reported");
    }

    /// A stream cancelled through its token while a consumer still reads it
    /// ends in an error, not in an empty `Complete`: a cut-off chat or load
    /// must not read as a finished one.
    #[tokio::test]
    async fn a_cancelled_relay_does_not_report_complete() {
        use wafer_block::stream::StreamEvent;

        let (service, chat_entered, _chat_dropped, _load_dropped) = Stuck::service(false);
        let mut chat_out = run(&service, ServiceOp::LLM_CHAT, &chat_body()).await;
        let mut load_out = run(&service, ServiceOp::LLM_LOAD_MODEL, &load_body()).await;
        within("the chat reaches the service", chat_entered)
            .await
            .expect("entered");

        for (name, out) in [("chat", &mut chat_out), ("load", &mut load_out)] {
            out.cancel_token().cancel();
            let ending = within("the cancelled stream ends", futures::StreamExt::next(out)).await;
            assert!(
                matches!(ending, Some(StreamEvent::Error(_))),
                "{name}: a cancelled relay ends in an error, got {ending:?}"
            );
        }
    }
}
