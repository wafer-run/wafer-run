//! The `wafer-run/vector` block, driven through the runtime: what it serves
//! is exactly what its declared `vector@v1` interface admits.
//!
//! Calls from another block pass the runtime's interface-action check in
//! `call_block`, which admits only the declared interface's actions. An op
//! the block's handler serves outside that interface is therefore
//! unreachable from any block, while still answering a top-level
//! `run_block`, which does not run that check. These tests pin both sides:
//! every `vector@v1` op reaches the service from a caller block, and every
//! other service op is refused by the block itself.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use wafer_block::{
    common::ServiceOp,
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    Block, BlockInfo, ErrorCode,
};
use wafer_core::{
    clients::vector as vclient,
    interfaces::vector::service::{
        DescribeIndexResponse, DistanceMetric, MetadataFilter, Result as VResult, SearchMode,
        VectorEntry, VectorIndexConfig, VectorMatch, VectorService,
    },
    service_blocks::vector::VectorBlock,
};
use wafer_run::{Context, Wafer};

/// A block using the vector service for indexes in its own namespace.
const CALLER: &str = "acme/indexer";
/// An index in [`CALLER`]'s namespace.
const INDEX: &str = "acme__indexer__docs";
/// The same index under a legacy uppercase spelling, for `rename_index`.
const LEGACY_INDEX: &str = "acme__indexer__Docs";

/// Records every op that reached it, in order.
#[derive(Default)]
struct RecordingVector {
    ops: Mutex<Vec<&'static str>>,
}

impl RecordingVector {
    fn record(&self, op: &'static str) {
        self.ops.lock().expect("ops lock").push(op);
    }
}

#[async_trait]
impl VectorService for RecordingVector {
    async fn create_index(&self, _config: VectorIndexConfig) -> VResult<()> {
        self.record(ServiceOp::VECTOR_CREATE_INDEX);
        Ok(())
    }
    async fn delete_index(&self, _name: &str) -> VResult<()> {
        self.record(ServiceOp::VECTOR_DELETE_INDEX);
        Ok(())
    }
    async fn upsert(&self, _index: &str, _entries: Vec<VectorEntry>) -> VResult<()> {
        self.record(ServiceOp::VECTOR_UPSERT);
        Ok(())
    }
    async fn query(
        &self,
        _index: &str,
        _vector: Vec<f32>,
        _top_k: usize,
        _filter: Option<MetadataFilter>,
        _mode: SearchMode,
        _keyword_query: Option<String>,
    ) -> VResult<Vec<VectorMatch>> {
        self.record(ServiceOp::VECTOR_QUERY);
        Ok(Vec::new())
    }
    async fn delete(&self, _index: &str, _ids: Vec<String>) -> VResult<()> {
        self.record(ServiceOp::VECTOR_DELETE);
        Ok(())
    }
    async fn count(&self, _index: &str) -> VResult<u64> {
        self.record(ServiceOp::VECTOR_COUNT);
        Ok(0)
    }
    async fn rename_index(&self, _from: &str, _to: &str) -> VResult<()> {
        self.record(ServiceOp::VECTOR_RENAME_INDEX);
        Ok(())
    }
    async fn list_indexes(&self, _prefix: &str) -> VResult<Vec<String>> {
        self.record(ServiceOp::VECTOR_LIST_INDEXES);
        Ok(Vec::new())
    }
    async fn describe_index(&self, _index: &str) -> VResult<DescribeIndexResponse> {
        self.record(ServiceOp::VECTOR_DESCRIBE_INDEX);
        Ok(DescribeIndexResponse {
            exists: false,
            columns: Vec::new(),
            keyword_search: false,
        })
    }
    async fn list_ids(&self, _index: &str, _filter: MetadataFilter) -> VResult<Vec<String>> {
        self.record(ServiceOp::VECTOR_LIST_IDS);
        Ok(Vec::new())
    }
}

/// Runs the vector op named by the message kind through the typed client,
/// from its own context. An op without an arm here fails the test, so a new
/// `vector@v1` op cannot be left out of it.
struct Caller;

#[async_trait]
impl Block for Caller {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(CALLER, "0.1.0", "test/iface@v1", "uses the vector block")
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let filter = || MetadataFilter {
            equals: [("k".to_string(), serde_json::json!("v"))].into(),
        };
        let result: Result<(), WaferError> = match msg.kind.as_str() {
            ServiceOp::VECTOR_CREATE_INDEX => {
                vclient::create_index(
                    ctx,
                    VectorIndexConfig {
                        name: INDEX.into(),
                        model: "m".into(),
                        dimensions: 1,
                        metric: DistanceMetric::Cosine,
                        keyword_search: false,
                    },
                )
                .await
            }
            ServiceOp::VECTOR_DELETE_INDEX => vclient::delete_index(ctx, INDEX).await,
            ServiceOp::VECTOR_UPSERT => {
                vclient::upsert(
                    ctx,
                    INDEX,
                    vec![VectorEntry {
                        id: "1".into(),
                        vector: vec![0.0],
                        metadata: None,
                        text: None,
                    }],
                )
                .await
            }
            ServiceOp::VECTOR_QUERY => {
                vclient::query(ctx, INDEX, vec![0.0], 1, None, SearchMode::Vector, None)
                    .await
                    .map(drop)
            }
            ServiceOp::VECTOR_DELETE => vclient::delete(ctx, INDEX, vec!["1".into()]).await,
            ServiceOp::VECTOR_COUNT => vclient::count(ctx, INDEX).await.map(drop),
            ServiceOp::VECTOR_LIST_INDEXES => vclient::list_indexes(ctx, "acme__indexer__")
                .await
                .map(drop),
            ServiceOp::VECTOR_DESCRIBE_INDEX => vclient::describe_index(ctx, INDEX).await.map(drop),
            ServiceOp::VECTOR_LIST_IDS => vclient::list_ids(ctx, INDEX, filter()).await.map(drop),
            ServiceOp::VECTOR_RENAME_INDEX => vclient::rename_index(ctx, LEGACY_INDEX, INDEX).await,
            other => Err(WaferError::new(
                ErrorCode::Unimplemented,
                format!("no test call for {other}"),
            )),
        };
        match result {
            Ok(()) => OutputStream::respond(Vec::new()),
            Err(e) => OutputStream::error(e),
        }
    }
}

async fn build() -> (Wafer, Arc<RecordingVector>) {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    let vector = Arc::new(RecordingVector::default());
    wafer_core::service_blocks::vector::register_with(&mut wafer, vector.clone())
        .expect("register wafer-run/vector");
    wafer
        .register_block(CALLER, Arc::new(Caller))
        .expect("register the caller");
    wafer.seal().await.expect("seal");
    (wafer, vector)
}

/// The error `out` ended in, or `None` for a response.
async fn error_of(out: OutputStream) -> Option<WaferError> {
    match out.collect_buffered().await {
        Ok(_) => None,
        Err(TerminalNotResponse::Error(e)) => Some(e),
        Err(other) => panic!("no response or error: {other:?}"),
    }
}

/// Every `vector@v1` op passes the runtime's interface-action check and
/// WRAP, and reaches the service. A guard: the vector ops were reachable
/// before the embedding half was removed too.
#[tokio::test]
async fn every_vector_op_reaches_the_service_from_a_caller_block() {
    let (wafer, vector) = build().await;

    for op in ServiceOp::VECTOR_OPS {
        let out = wafer
            .run_block(
                CALLER,
                Message::new(*op),
                InputStream::from_bytes(Vec::new()),
            )
            .await;
        if let Some(e) = error_of(out).await {
            panic!("{op} from {CALLER} failed: {:?} {}", e.code, e.message);
        }
    }

    assert_eq!(
        *vector.ops.lock().expect("ops lock"),
        ServiceOp::VECTOR_OPS.to_vec(),
    );
}

/// The block serves exactly the actions of the interface it declares:
/// every op of every service family outside `vector@v1` answers
/// `Unimplemented` from the block itself, and every `vector@v1` op is
/// dispatched to the vector handler. Sent with `run_block`, which skips the
/// interface-action check, so it is the block's own routing under test —
/// an op it served beyond its interface would be unreachable from any
/// caller block, which is how `embedding.*` sat dead here.
#[tokio::test]
async fn the_vector_block_serves_exactly_its_declared_interface() {
    let (wafer, vector) = build().await;
    let spec = wafer_block::interfaces::vector_v1();
    assert_eq!(
        VectorBlock::new(Arc::new(RecordingVector::default()))
            .info()
            .interface,
        spec.name,
    );

    let families: [&[&str]; 11] = [
        ServiceOp::DATABASE_OPS,
        ServiceOp::VECTOR_OPS,
        ServiceOp::STORAGE_OPS,
        ServiceOp::CRYPTO_OPS,
        ServiceOp::NETWORK_OPS,
        ServiceOp::LOGGER_OPS,
        ServiceOp::AUTH_OPS,
        ServiceOp::EMBEDDING_OPS,
        ServiceOp::LLM_OPS,
        ServiceOp::IMAGE_OPS,
        ServiceOp::CONFIG_OPS,
    ];
    for op in families.into_iter().flatten() {
        let out = wafer
            .run_block(
                VectorBlock::NAME,
                Message::new(*op),
                InputStream::from_bytes(b"{}".to_vec()),
            )
            .await;
        let unknown = error_of(out).await.is_some_and(|e| {
            e.code == ErrorCode::Unimplemented && e.message.contains("unknown vector operation")
        });
        let declared = spec.actions.contains_key(*op);
        assert_eq!(
            unknown, !declared,
            "{op}: declared in vector@v1 = {declared}, refused as unknown = {unknown}"
        );
    }
    // The empty bodies fail to decode before any service method runs.
    assert!(vector.ops.lock().expect("ops lock").is_empty());
}
