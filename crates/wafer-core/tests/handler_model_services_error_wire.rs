//! What a caller of the llm and image handlers reads when the service fails
//! with `Unavailable`.
//!
//! A provider that cannot be reached (`Network`) and an engine that cannot
//! take the request (`EngineUnavailable`) both reach the caller as
//! `ErrorCode::Unavailable`. Only the second carries a message written for
//! the caller, so an edge (an HTTP route, say) can pass `Unavailable`
//! through only if the first never carries the provider client's own text.
//! These tests drive the real handlers, every op of both families, with a
//! service that fails the op, and read the terminal error off the stream.

use std::sync::Arc;

use futures::{stream::BoxStream, StreamExt};
use tokio_util::sync::CancellationToken;
use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    stream::StreamEvent,
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceAccess, ResourceType},
    wire::{image, llm, model_common},
    ErrorCode, Message, WaferError,
};
use wafer_core::interfaces::{
    image::{
        handler as image_handler,
        service::{ImageError, ImageService},
    },
    llm::{
        handler as llm_handler,
        service::{LlmError, LlmService},
    },
};

/// The admin block, admitted to every op by the real WRAP check.
const ADMIN: &str = "acme/admin";

/// Text a provider client puts in a transport error: a private host, port
/// and path. None of it may reach the caller.
const PROVIDER_TEXT: &str =
    "error sending request for url (http://10.0.0.5:11434/api/chat): dns failure for internal.corp";

/// A reason an engine wrote for the caller. It must reach them intact.
const ENGINE_REASON: &str = "no open page has the model 'm' loaded — load it first";

/// The admin block's call into the serving block, authorized through the
/// real WRAP check.
struct AdminCtx;

#[wafer_block::wafer_async_trait]
impl Context for AdminCtx {
    async fn call_block(&self, _b: &str, _m: Message, _i: InputStream) -> OutputStream {
        unimplemented!("the model handlers make no calls")
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("the model handlers keep no context")
    }
    fn caller_id(&self) -> Option<&str> {
        Some(ADMIN)
    }
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: ResourceType,
        access: ResourceAccess,
    ) -> Result<(), WaferError> {
        wafer_block::wrap::check_access(
            Some(ADMIN),
            resource,
            access,
            Some(&resource_type),
            &[],
            ADMIN,
        )
    }
    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: ResourceType,
        access: ResourceAccess,
    ) -> bool {
        self.check_resource_access(resource, resource_type, access)
            .is_ok()
    }
}

/// How the failing service fails.
#[derive(Clone, Copy)]
enum Failure {
    ProviderUnreachable,
    EngineUnavailable,
}

impl Failure {
    fn llm(self) -> LlmError {
        match self {
            Failure::ProviderUnreachable => LlmError::Network(PROVIDER_TEXT.into()),
            Failure::EngineUnavailable => LlmError::EngineUnavailable(ENGINE_REASON.into()),
        }
    }
    fn image(self) -> ImageError {
        match self {
            Failure::ProviderUnreachable => ImageError::Network(PROVIDER_TEXT.into()),
            Failure::EngineUnavailable => ImageError::EngineUnavailable(ENGINE_REASON.into()),
        }
    }
}

/// A service that fails every op with one [`Failure`]. The streaming ops
/// fail as the stream's first item, the buffered ops as their result.
struct Failing(Failure);

#[async_trait::async_trait]
impl LlmService for Failing {
    async fn chat_stream(
        &self,
        _req: llm::ChatRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<llm::ChatChunk, LlmError>> {
        Box::pin(futures::stream::iter([Err(self.0.llm())]))
    }
    async fn list_models(&self) -> Result<Vec<llm::ModelInfo>, LlmError> {
        Err(self.0.llm())
    }
    async fn status(&self, _b: &str, _m: &str) -> Result<model_common::ModelStatus, LlmError> {
        Err(self.0.llm())
    }
    fn load_model(
        &self,
        _b: &str,
        _m: &str,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<model_common::LoadProgress, LlmError>> {
        Box::pin(futures::stream::iter([Err(self.0.llm())]))
    }
    async fn unload_model(&self, _b: &str, _m: &str) -> Result<(), LlmError> {
        Err(self.0.llm())
    }
}

#[async_trait::async_trait]
impl ImageService for Failing {
    async fn generate(
        &self,
        _req: image::ImageRequest,
        _cancel: CancellationToken,
    ) -> Result<image::ImageResponse, ImageError> {
        Err(self.0.image())
    }
    async fn list_models(&self) -> Result<Vec<image::ModelInfo>, ImageError> {
        Err(self.0.image())
    }
    async fn status(&self, _b: &str, _m: &str) -> Result<model_common::ModelStatus, ImageError> {
        Err(self.0.image())
    }
    fn load_model(
        &self,
        _b: &str,
        _m: &str,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<model_common::LoadProgress, ImageError>> {
        Box::pin(futures::stream::iter([Err(self.0.image())]))
    }
    async fn unload_model(&self, _b: &str, _m: &str) -> Result<(), ImageError> {
        Err(self.0.image())
    }
}

/// The body of an llm or image op naming backend `b`'s model `m`.
fn body(op: &str) -> Vec<u8> {
    match op {
        ServiceOp::LLM_CHAT => codec::encode(&llm::ChatRequest::new("b", "m", Vec::new())),
        ServiceOp::IMAGE_GENERATE => codec::encode(&image::ImageRequest::new("b", "m", "a cat")),
        ServiceOp::LLM_LIST_MODELS | ServiceOp::IMAGE_LIST_MODELS => Ok(Vec::new()),
        ServiceOp::LLM_STATUS | ServiceOp::IMAGE_STATUS => {
            codec::encode(&model_common::StatusRequest {
                backend_id: "b".into(),
                model_id: "m".into(),
            })
        }
        ServiceOp::LLM_LOAD_MODEL | ServiceOp::IMAGE_LOAD_MODEL => {
            codec::encode(&model_common::LoadModelRequest {
                backend_id: "b".into(),
                model_id: "m".into(),
            })
        }
        ServiceOp::LLM_UNLOAD_MODEL | ServiceOp::IMAGE_UNLOAD_MODEL => {
            codec::encode(&model_common::UnloadModelRequest {
                backend_id: "b".into(),
                model_id: "m".into(),
            })
        }
        other => panic!("no body for `{other}`"),
    }
    .expect("encode")
}

/// Run `op` against a service failing with `failure` and return the error
/// the stream ended with.
async fn terminal_error(op: &str, failure: Failure) -> WaferError {
    let msg = Message::new(op);
    let body = body(op);
    let out = if op.starts_with("llm.") {
        let svc: Arc<dyn LlmService> = Arc::new(Failing(failure));
        llm_handler::handle_message(&svc, &AdminCtx, "wafer-run/llm", &msg, &body).await
    } else {
        let svc: Arc<dyn ImageService> = Arc::new(Failing(failure));
        image_handler::handle_message(&svc, &AdminCtx, "wafer-run/image", &msg, &body).await
    };
    for event in out.collect::<Vec<_>>().await {
        if let StreamEvent::Error(e) = event {
            return *e;
        }
    }
    panic!("{op}: the stream ended without an error");
}

/// Every op of both families, with the message a caller is told when the
/// provider cannot be reached.
fn ops() -> impl Iterator<Item = (&'static str, &'static str)> {
    ServiceOp::LLM_OPS
        .iter()
        .map(|op| (*op, llm_handler::PROVIDER_UNREACHABLE))
        .chain(
            ServiceOp::IMAGE_OPS
                .iter()
                .map(|op| (*op, image_handler::PROVIDER_UNREACHABLE)),
        )
}

/// A provider that cannot be reached is `Unavailable`, and the caller reads
/// that the provider could not be reached, not the provider client's text.
#[tokio::test]
async fn an_unreachable_provider_is_unavailable_without_its_text() {
    for (op, unreachable) in ops() {
        let e = terminal_error(op, Failure::ProviderUnreachable).await;
        assert_eq!(e.code, ErrorCode::Unavailable, "{op}: {e:?}");
        assert!(e.message.ends_with(unreachable), "{op}: {}", e.message);
        for leaked in [
            "10.0.0.5",
            "11434",
            "internal.corp",
            "error sending request",
        ] {
            assert!(
                !e.message.contains(leaked),
                "{op}: the provider's text reached the caller: {}",
                e.message
            );
        }
    }
}

/// An engine that cannot take the request is `Unavailable` too, and its
/// reason, written for the caller, reaches them word for word.
#[tokio::test]
async fn an_unavailable_engine_reaches_the_caller_with_its_reason() {
    for (op, _) in ops() {
        let e = terminal_error(op, Failure::EngineUnavailable).await;
        assert_eq!(e.code, ErrorCode::Unavailable, "{op}: {e:?}");
        assert!(e.message.ends_with(ENGINE_REASON), "{op}: {}", e.message);
    }
}
