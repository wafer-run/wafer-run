//! The llm and embedding services authorize their caller, driven end to
//! end: caller blocks use the typed `wafer_core::clients::{llm, vector}`
//! clients through `ctx.call_block` to the REAL `wafer-run/llm` block and
//! an embedding block serving the shared embedding handler, whose handlers
//! authorize the caller the REAL `RuntimeContext` names.
//!
//! The grants are declared the way an embedder declares them — on the llm
//! service (`MultiBackendLlmService::grant`), embedded in the llm block's
//! info, and in the embedding block's own info — so the runtime's grant
//! registration and its namespace-ownership check are on the path too.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceGrant, ResourceType},
    Block, BlockInfo, ErrorCode,
};
use wafer_core::{
    clients::llm::{ChatChunk, ChatRequest, ModelInfo, ModelStatus, UnloadModelRequest},
    interfaces::{
        llm::{
            router::MultiBackendLlmService,
            service::{LlmError, LlmService},
        },
        vector::service::{EmbeddingService, Result as VResult},
    },
};
use wafer_run::{Context, RuntimeError, Wafer};

/// Holds read grants on the llm block's models and on `embed`.
const GRANTED: &str = "acme/assistant";
/// Holds no grant.
const FEATURE: &str = "acme/feature";

/// Counts the paid calls a provider backend would bill.
#[derive(Default)]
struct Provider {
    chats: AtomicUsize,
    unloads: AtomicUsize,
}

#[async_trait]
impl LlmService for Provider {
    async fn chat_stream(
        &self,
        _req: ChatRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ChatChunk, LlmError>> {
        self.chats.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter([Ok(ChatChunk::text("hi"))]))
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(Vec::new())
    }
    async fn status(&self, _b: &str, _m: &str) -> Result<ModelStatus, LlmError> {
        Ok(ModelStatus::ready())
    }
    async fn unload_model(&self, _b: &str, _m: &str) -> Result<(), LlmError> {
        self.unloads.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn claims_backend(&self, backend_id: &str) -> bool {
        backend_id == "provider"
    }
}

/// Counts embeddings.
#[derive(Default)]
struct Embedder {
    embeds: AtomicUsize,
}

#[async_trait]
impl EmbeddingService for Embedder {
    fn model(&self) -> &str {
        "m"
    }
    fn dimensions(&self) -> u32 {
        1
    }
    async fn embed(&self, texts: Vec<String>) -> VResult<Vec<Vec<f32>>> {
        self.embeds.fetch_add(1, Ordering::SeqCst);
        Ok(texts.iter().map(|_| vec![0.0]).collect())
    }
}

/// An embedding block like an embedder's own (`embedding@v1`): it serves
/// [`Embedder`] through the shared embedding handler under its registered
/// name, and grants `embed` to [`GRANTED`].
struct EmbeddingBlock(Arc<Embedder>);

impl EmbeddingBlock {
    const NAME: &'static str = "acme/embedder";
}

#[async_trait]
impl Block for EmbeddingBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(Self::NAME, "0.1.0", "embedding@v1", "embeds text")
            .grants(vec![ResourceGrant::read(GRANTED, "acme__embedder__embed")
                .typed(ResourceType::Embedding)])
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return OutputStream::error(e),
        };
        wafer_core::interfaces::vector::handler::handle_embedding_message(
            self.0.as_ref(),
            ctx,
            Self::NAME,
            &msg,
            &body,
        )
        .await
    }
}

/// A caller block: `chat` chats with the provider's model, `unload` unloads
/// it, `embed` embeds one text — each through the typed client, from the
/// block's own context. `grants` is what the block itself declares.
struct Caller {
    name: &'static str,
    grants: Vec<ResourceGrant>,
}

#[async_trait]
impl Block for Caller {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            self.name,
            "0.1.0",
            "test/iface@v1",
            "calls llm and embedding",
        )
        .grants(self.grants.clone())
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let done = |r: Result<(), WaferError>| match r {
            Ok(()) => OutputStream::respond(Vec::new()),
            Err(e) => OutputStream::error(e),
        };
        match msg.kind.as_str() {
            "chat" => done(
                wafer_core::clients::llm::chat(ctx, &ChatRequest::new("provider", "m", Vec::new()))
                    .await
                    .map(drop),
            ),
            "unload" => done(
                wafer_core::clients::llm::unload_model(
                    ctx,
                    &UnloadModelRequest {
                        backend_id: "provider".into(),
                        model_id: "m".into(),
                    },
                )
                .await,
            ),
            "embed" => done(
                wafer_core::clients::vector::embed(ctx, EmbeddingBlock::NAME, vec!["hi".into()])
                    .await
                    .map(drop),
            ),
            other => OutputStream::error(WaferError::new(ErrorCode::Unimplemented, other)),
        }
    }
}

async fn build_with(
    feature_grants: Vec<ResourceGrant>,
) -> Result<(Arc<Wafer>, Arc<Provider>, Arc<Embedder>), RuntimeError> {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    let provider = Arc::new(Provider::default());
    let mut router = MultiBackendLlmService::new();
    router
        .grant(ResourceGrant::read(GRANTED, "wafer_run__llm__provider/*").typed(ResourceType::Llm))
        .register("provider", provider.clone());
    wafer_core::service_blocks::llm::register_with(&mut wafer, Arc::new(router))
        .expect("register wafer-run/llm");
    let embedder = Arc::new(Embedder::default());
    wafer
        .register_block(
            EmbeddingBlock::NAME,
            Arc::new(EmbeddingBlock(embedder.clone())),
        )
        .expect("register the embedding block");
    wafer
        .register_block(
            GRANTED,
            Arc::new(Caller {
                name: GRANTED,
                grants: Vec::new(),
            }),
        )
        .expect("register granted caller");
    wafer
        .register_block(
            FEATURE,
            Arc::new(Caller {
                name: FEATURE,
                grants: feature_grants,
            }),
        )
        .expect("register feature caller");
    wafer.seal().await?;
    Ok((Arc::new(wafer), provider, embedder))
}

async fn build() -> (Arc<Wafer>, Arc<Provider>, Arc<Embedder>) {
    build_with(Vec::new()).await.expect("seal")
}

/// Run `kind` as `caller`: `Ok` or the code it failed with.
async fn run(wafer: &Wafer, caller: &str, kind: &str) -> Result<(), ErrorCode> {
    let out = wafer
        .run_block(
            caller,
            Message::new(kind),
            InputStream::from_bytes(Vec::new()),
        )
        .await;
    match out.collect_buffered().await {
        Ok(_) => Ok(()),
        Err(TerminalNotResponse::Error(e)) => Err(e.code),
        Err(other) => panic!("no response or error: {other:?}"),
    }
}

/// The finding: any block that could reach `wafer-run/llm` could spend the
/// provider's paid quota. A block the llm block did not grant is refused,
/// and the provider is never called.
#[tokio::test]
async fn chat_is_refused_to_a_block_without_the_llm_blocks_grant() {
    let (wafer, provider, _) = build().await;

    assert_eq!(
        run(&wafer, FEATURE, "chat").await,
        Err(ErrorCode::PermissionDenied)
    );
    assert_eq!(provider.chats.load(Ordering::SeqCst), 0);
}

/// The llm block's grant admits its grantee — the runtime registers the
/// typed `Llm` grant the router declares on the llm block's namespace.
#[tokio::test]
async fn chat_is_served_to_the_block_the_llm_block_granted() {
    let (wafer, provider, _) = build().await;

    assert_eq!(run(&wafer, GRANTED, "chat").await, Ok(()));
    assert_eq!(provider.chats.load(Ordering::SeqCst), 1);
}

/// Unloading a model changes what every other caller finds loaded: a
/// read grant to use the model does not admit it.
#[tokio::test]
async fn a_read_grantee_cannot_unload_a_model() {
    let (wafer, provider, _) = build().await;

    assert_eq!(
        run(&wafer, GRANTED, "unload").await,
        Err(ErrorCode::PermissionDenied)
    );
    assert_eq!(provider.unloads.load(Ordering::SeqCst), 0);
}

/// Embedding needs the grant the embedding block declares on its own
/// namespace.
#[tokio::test]
async fn embed_needs_the_embedding_services_grant() {
    let (wafer, _, embedder) = build().await;

    assert_eq!(
        run(&wafer, FEATURE, "embed").await,
        Err(ErrorCode::PermissionDenied)
    );
    assert_eq!(embedder.embeds.load(Ordering::SeqCst), 0);
    assert_eq!(run(&wafer, GRANTED, "embed").await, Ok(()));
    assert_eq!(embedder.embeds.load(Ordering::SeqCst), 1);
}

/// Only the llm block can grant its models: a block that declares a grant
/// on the llm block's namespace for itself is refused at seal.
#[tokio::test]
async fn a_block_cannot_grant_itself_the_llm_blocks_models() {
    let result = build_with(vec![ResourceGrant::read_write(
        FEATURE,
        "wafer_run__llm__*",
    )
    .typed(ResourceType::Llm)])
    .await;

    match result {
        Err(RuntimeError::GrantsRejected(rejected)) => {
            assert_eq!(rejected.len(), 1, "{rejected:?}");
            assert_eq!(rejected[0].block, FEATURE);
        }
        Err(other) => panic!("expected GrantsRejected, got {other:?}"),
        Ok(_) => panic!("a self-declared grant on another block's namespace was accepted"),
    }
}
