//! Authorization of the llm, image and embedding handlers under the REAL
//! WRAP check (`wafer_block::wrap::check_access`).
//!
//! Each handler authorizes every op against a resource in the serving
//! block's own namespace before the service runs. These tests drive the real
//! handlers with a `Context` that authorizes through `check_access` with the
//! grants a serving block would declare, and a recording service, checking
//! who is admitted to what: a grantee, the admin block, and — refused — a
//! block without the grant, a read-only grantee loading or unloading a
//! model, a grantee of one backend reaching another, and a backend id that
//! cannot name a resource.

use std::sync::{Arc, Mutex};

use futures::{stream::BoxStream, StreamExt};
use tokio_util::sync::CancellationToken;
use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceAccess, ResourceGrant, ResourceType},
    wire::{image, llm, model_common, vector},
    ErrorCode, Message, WaferError,
};
use wafer_core::interfaces::{
    image::service::{ImageError, ImageService},
    llm::service::{LlmError, LlmService},
    vector::service::{EmbeddingService, Result as VResult},
};

const ADMIN: &str = "acme/admin";
/// A block granted by the serving block.
const GRANTEE: &str = "acme/chat";
/// A block holding no grant.
const FEATURE: &str = "acme/feature";

/// The serving block's context for one call from `caller`, authorizing
/// through the real WRAP check with the serving block's grants.
struct WrapCtx {
    caller: &'static str,
    grants: Vec<ResourceGrant>,
}

impl WrapCtx {
    fn new(caller: &'static str, grants: Vec<ResourceGrant>) -> Self {
        Self { caller, grants }
    }
}

#[wafer_block::wafer_async_trait]
impl Context for WrapCtx {
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
        Some(self.caller)
    }
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: ResourceType,
        access: ResourceAccess,
    ) -> Result<(), WaferError> {
        wafer_block::wrap::check_access(
            Some(self.caller),
            resource,
            access,
            Some(&resource_type),
            &self.grants,
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

/// Records every service method that runs.
#[derive(Default)]
struct Recording {
    calls: Mutex<Vec<&'static str>>,
}

impl Recording {
    fn record(&self, op: &'static str) {
        self.calls.lock().unwrap().push(op);
    }
    fn ran(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl LlmService for Recording {
    async fn chat_stream(
        &self,
        _req: llm::ChatRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<llm::ChatChunk, LlmError>> {
        self.record("chat");
        Box::pin(futures::stream::empty())
    }
    async fn list_models(&self) -> Result<Vec<llm::ModelInfo>, LlmError> {
        self.record("list_models");
        Ok(Vec::new())
    }
    async fn status(&self, _b: &str, _m: &str) -> Result<model_common::ModelStatus, LlmError> {
        self.record("status");
        Ok(model_common::ModelStatus::ready())
    }
    fn load_model(
        &self,
        _b: &str,
        _m: &str,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<model_common::LoadProgress, LlmError>> {
        self.record("load_model");
        Box::pin(futures::stream::empty())
    }
    async fn unload_model(&self, _b: &str, _m: &str) -> Result<(), LlmError> {
        self.record("unload_model");
        Ok(())
    }
}

#[async_trait::async_trait]
impl ImageService for Recording {
    async fn generate(
        &self,
        _req: image::ImageRequest,
        _cancel: CancellationToken,
    ) -> Result<image::ImageResponse, ImageError> {
        self.record("generate");
        Ok(image::ImageResponse { images: Vec::new() })
    }
    async fn list_models(&self) -> Result<Vec<image::ModelInfo>, ImageError> {
        self.record("list_models");
        Ok(Vec::new())
    }
    async fn status(&self, _b: &str, _m: &str) -> Result<model_common::ModelStatus, ImageError> {
        self.record("status");
        Ok(model_common::ModelStatus::ready())
    }
    fn load_model(
        &self,
        _b: &str,
        _m: &str,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<model_common::LoadProgress, ImageError>> {
        self.record("load_model");
        Box::pin(futures::stream::empty())
    }
    async fn unload_model(&self, _b: &str, _m: &str) -> Result<(), ImageError> {
        self.record("unload_model");
        Ok(())
    }
}

#[async_trait::async_trait]
impl EmbeddingService for Recording {
    fn model(&self) -> &str {
        "m"
    }
    fn dimensions(&self) -> u32 {
        1
    }
    async fn embed(&self, texts: Vec<String>) -> VResult<Vec<Vec<f32>>> {
        self.record("embed");
        Ok(texts.iter().map(|_| vec![0.0]).collect())
    }
    fn count_tokens(&self, _text: &str) -> usize {
        self.record("count_tokens");
        1
    }
}

/// The body of an llm or image op naming `backend`'s model `m`.
fn body(op: &str, backend: &str) -> Vec<u8> {
    let model = |b: &str| model_common::StatusRequest {
        backend_id: b.into(),
        model_id: "m".into(),
    };
    match op {
        ServiceOp::LLM_CHAT => codec::encode(&llm::ChatRequest::new(backend, "m", Vec::new())),
        ServiceOp::IMAGE_GENERATE => {
            codec::encode(&image::ImageRequest::new(backend, "m", "a cat"))
        }
        ServiceOp::LLM_LIST_MODELS | ServiceOp::IMAGE_LIST_MODELS => Ok(Vec::new()),
        ServiceOp::LLM_STATUS | ServiceOp::IMAGE_STATUS => codec::encode(&model(backend)),
        ServiceOp::LLM_LOAD_MODEL | ServiceOp::IMAGE_LOAD_MODEL => {
            codec::encode(&model_common::LoadModelRequest {
                backend_id: backend.into(),
                model_id: "m".into(),
            })
        }
        ServiceOp::LLM_UNLOAD_MODEL | ServiceOp::IMAGE_UNLOAD_MODEL => {
            codec::encode(&model_common::UnloadModelRequest {
                backend_id: backend.into(),
                model_id: "m".into(),
            })
        }
        ServiceOp::EMBEDDING_EMBED => codec::encode(&vector::EmbedRequest {
            texts: vec!["hi".into()],
        }),
        ServiceOp::EMBEDDING_COUNT_TOKENS => {
            codec::encode(&vector::CountTokensRequest { text: "hi".into() })
        }
        other => panic!("no body for `{other}`"),
    }
    .expect("encode")
}

/// Run `op` as served by `block` under `ctx`: `Ok` once the whole stream
/// completed, or the error code it ended with.
async fn run(
    svc: &Arc<Recording>,
    ctx: &WrapCtx,
    block: &str,
    op: &str,
    backend: &str,
) -> Result<(), ErrorCode> {
    let msg = Message::new(op);
    let body = body(op, backend);
    let out = if op.starts_with("llm.") {
        let svc: Arc<dyn LlmService> = svc.clone();
        wafer_core::interfaces::llm::handler::handle_message(&svc, ctx, block, &msg, &body).await
    } else if op.starts_with("image.") {
        let svc: Arc<dyn ImageService> = svc.clone();
        wafer_core::interfaces::image::handler::handle_message(&svc, ctx, block, &msg, &body).await
    } else {
        wafer_core::interfaces::vector::handler::handle_embedding_message(
            svc.as_ref(),
            ctx,
            block,
            &msg,
            &body,
        )
        .await
    };
    for event in out.collect::<Vec<_>>().await {
        if let wafer_block::stream::StreamEvent::Error(e) = event {
            return Err(e.code);
        }
    }
    Ok(())
}

/// The model families: `(serving block, resource type, ops)`.
fn families() -> [(&'static str, ResourceType, &'static [&'static str]); 2] {
    [
        ("wafer-run/llm", ResourceType::Llm, ServiceOp::LLM_OPS),
        ("wafer-run/image", ResourceType::Image, ServiceOp::IMAGE_OPS),
    ]
}

fn is_load_or_unload(op: &str) -> bool {
    op.ends_with(".load_model") || op.ends_with(".unload_model")
}

/// The finding: any block that could reach the llm or image block could
/// spend its quota or unload its models. A block the serving block did not
/// grant is refused every op, and the service never runs.
#[tokio::test]
async fn a_block_without_the_serving_blocks_grant_is_refused_every_op() {
    let ctx = WrapCtx::new(FEATURE, Vec::new());
    for (block, _, ops) in families() {
        for op in ops {
            let svc = Arc::new(Recording::default());
            assert_eq!(
                run(&svc, &ctx, block, op, "b").await,
                Err(ErrorCode::PermissionDenied),
                "{op}"
            );
            assert!(svc.ran().is_empty(), "{op} reached the service");
        }
    }
    let svc = Arc::new(Recording::default());
    for op in ServiceOp::EMBEDDING_OPS {
        assert_eq!(
            run(&svc, &ctx, "acme/embedder", op, "b").await,
            Err(ErrorCode::PermissionDenied),
            "{op}"
        );
    }
    assert!(svc.ran().is_empty());
}

/// A read grant on the serving block's namespace lets its grantee use every
/// model and list them, but not load or unload one: that changes what every
/// other caller finds loaded.
#[tokio::test]
async fn a_read_grant_uses_models_but_cannot_load_or_unload_them() {
    for (block, rt, ops) in families() {
        let pattern = format!("{}*", wafer_block::wrap::resource_prefix(block));
        let ctx = WrapCtx::new(
            GRANTEE,
            vec![ResourceGrant::read(GRANTEE, &pattern).typed(rt)],
        );
        for op in ops {
            let svc = Arc::new(Recording::default());
            let result = run(&svc, &ctx, block, op, "b").await;
            if is_load_or_unload(op) {
                assert_eq!(result, Err(ErrorCode::PermissionDenied), "{op}");
                assert!(svc.ran().is_empty(), "{op} reached the service");
            } else {
                assert_eq!(result, Ok(()), "{op}");
                assert_eq!(svc.ran().len(), 1, "{op}");
            }
        }
    }
}

/// The admin block is admitted to every op, loading and unloading included.
#[tokio::test]
async fn the_admin_block_is_admitted_to_every_op() {
    let ctx = WrapCtx::new(ADMIN, Vec::new());
    for (block, _, ops) in families() {
        for op in ops {
            let svc = Arc::new(Recording::default());
            assert_eq!(run(&svc, &ctx, block, op, "b").await, Ok(()), "{op}");
        }
    }
    for op in ServiceOp::EMBEDDING_OPS {
        let svc = Arc::new(Recording::default());
        assert_eq!(
            run(&svc, &ctx, "acme/embedder", op, "b").await,
            Ok(()),
            "{op}"
        );
    }
}

/// A grant on one backend's models admits those models only.
#[tokio::test]
async fn a_grant_on_one_backend_does_not_reach_another() {
    let ctx = WrapCtx::new(
        GRANTEE,
        vec![ResourceGrant::read(GRANTEE, "wafer_run__llm__openai/*").typed(ResourceType::Llm)],
    );
    let svc = Arc::new(Recording::default());
    assert_eq!(
        run(&svc, &ctx, "wafer-run/llm", ServiceOp::LLM_CHAT, "openai").await,
        Ok(())
    );
    assert_eq!(
        run(&svc, &ctx, "wafer-run/llm", ServiceOp::LLM_CHAT, "local").await,
        Err(ErrorCode::PermissionDenied)
    );
    assert_eq!(svc.ran(), vec!["chat"]);
}

/// A backend id containing `/` cannot name one backend's models
/// unambiguously (`openai/x` would sit under a grant on `openai/*`), so it
/// is refused as malformed before any check — even for the admin — and the
/// service never runs.
#[tokio::test]
async fn a_backend_id_with_a_slash_is_invalid_argument() {
    let ctx = WrapCtx::new(ADMIN, Vec::new());
    for (block, _, ops) in families() {
        for op in ops.iter().filter(|op| !op.ends_with(".list_models")) {
            let svc = Arc::new(Recording::default());
            assert_eq!(
                run(&svc, &ctx, block, op, "openai/x").await,
                Err(ErrorCode::InvalidArgument),
                "{op}"
            );
            assert!(svc.ran().is_empty(), "{op} reached the service");
        }
    }
}

/// Embedding is authorized per op, in the namespace of whichever block
/// serves it: a grant on `embed` does not admit `count_tokens`, and a grant
/// declared on one embedding block's namespace does not admit calls another
/// block serves.
#[tokio::test]
async fn embedding_is_authorized_per_op_in_the_serving_blocks_namespace() {
    let ctx = WrapCtx::new(
        GRANTEE,
        vec![ResourceGrant::read(GRANTEE, "acme__embedder__embed").typed(ResourceType::Embedding)],
    );
    let svc = Arc::new(Recording::default());
    assert_eq!(
        run(&svc, &ctx, "acme/embedder", ServiceOp::EMBEDDING_EMBED, "").await,
        Ok(())
    );
    assert_eq!(
        run(
            &svc,
            &ctx,
            "acme/embedder",
            ServiceOp::EMBEDDING_COUNT_TOKENS,
            ""
        )
        .await,
        Err(ErrorCode::PermissionDenied)
    );
    assert_eq!(
        run(
            &svc,
            &ctx,
            "acme/other-embedder",
            ServiceOp::EMBEDDING_EMBED,
            ""
        )
        .await,
        Err(ErrorCode::PermissionDenied)
    );
    assert_eq!(svc.ran(), vec!["embed"]);
}
