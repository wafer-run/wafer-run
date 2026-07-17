//! Streaming-UPLOAD dispatch tests for the wired `STORAGE_PUT_STREAMING` op.
//!
//! The upload direction is the mirror of the download direction
//! (`handler_streaming_download.rs`): here the REQUEST is the stream. Three
//! properties are pinned:
//!
//! 1. **Non-collapsing round-trip through dispatch** (op string → the
//!    macro-generated `StorageBlock::handle` → `handle_put_streaming` → the
//!    service's `put_streaming` → backend): the request body frames reach
//!    `put_streaming` **verbatim** as ordered chunks — the typed header frame
//!    is peeled off first, then each body chunk arrives as its own frame — and
//!    are never collapsed by `collect_to_bytes` (which is exactly what the
//!    streaming ingress exists to avoid). A recording fake proves the
//!    streaming service method ran, not the buffered `put`, and observed every
//!    distinct chunk.
//!
//! 2. **WRAP-authorization parity** (the security focus of this change): a
//!    recording `Context` captures the exact `(resource, resource_type,
//!    is_write)` tuple the op hands to `check_resource_access`, and the test
//!    asserts the streaming upload requests the **identical** grant tuple as
//!    the buffered `storage.put` — a WRITE of `{folder}/{key}` — and is denied
//!    identically when that grant is absent. So the streaming upload can never
//!    be reached with a weaker (or read-only) gate than the buffered upload.
//!
//! 3. **Additivity**: the buffered `storage.put` path through the same block is
//!    byte-for-byte unchanged — the additive stream-ingress branch only
//!    intercepts the streaming op.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::ResourceType,
    wire, Block, ErrorCode, Message, WaferError,
};
use wafer_core::{
    interfaces::storage::service::{
        FolderInfo, ListOptions, ObjectInfo, ObjectList, StorageError, StorageService,
    },
    service_blocks::storage::StorageBlock,
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// A bare `Message` carrying only `kind` — no WRAP meta. The recording
/// `Context` (not any message meta) is what gates the call.
fn msg(kind: &str) -> Message {
    Message::new(kind)
}

/// Build a streaming-put request `InputStream`: the encoded
/// [`wire::storage::PutStreamingHeader`] header frame followed by each body
/// chunk as its own distinct frame (so a collapse would be observable).
fn put_streaming_input(
    folder: &str,
    key: &str,
    content_type: &str,
    body_chunks: &[Vec<u8>],
) -> InputStream {
    let header = codec::encode(&wire::storage::PutStreamingHeader {
        folder: folder.to_string(),
        key: key.to_string(),
        content_type: content_type.to_string(),
    })
    .expect("encode PutStreamingHeader");
    let mut frames = vec![header];
    frames.extend(body_chunks.iter().cloned());
    InputStream::from_stream(futures::stream::iter(frames))
}

async fn expect_permission_denied(out: OutputStream) {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => assert_eq!(
            e.code,
            ErrorCode::PermissionDenied,
            "expected PERMISSION_DENIED, got {:?}: {}",
            e.code,
            e.message
        ),
        other => panic!("expected a PermissionDenied error terminal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// RecordingCtx — captures every `(resource, resource_type, is_write)` tuple
// handed to `check_resource_access`, and allows or denies uniformly. Used to
// prove the streaming op requests the SAME grant as the buffered op.
// ---------------------------------------------------------------------------

struct RecordingCtx {
    allow: bool,
    seen: Mutex<Vec<(String, ResourceType, bool)>>,
}

impl RecordingCtx {
    fn allow() -> Self {
        Self {
            allow: true,
            seen: Mutex::new(Vec::new()),
        }
    }
    fn deny() -> Self {
        Self {
            allow: false,
            seen: Mutex::new(Vec::new()),
        }
    }
    fn seen(&self) -> Vec<(String, ResourceType, bool)> {
        self.seen.lock().unwrap().clone()
    }
}

#[wafer_block::wafer_async_trait]
impl Context for RecordingCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("storage service is local; call_block is not exercised")
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("not exercised by these tests")
    }

    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: ResourceType,
        is_write: bool,
    ) -> Result<(), WaferError> {
        self.seen
            .lock()
            .unwrap()
            .push((resource.to_string(), resource_type, is_write));
        if self.allow {
            Ok(())
        } else {
            Err(WaferError::new(
                ErrorCode::PermissionDenied,
                "denied by test ctx",
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Recording storage fake — its `put_streaming` override drains the body
// `InputStream` chunk-by-chunk and records each distinct frame, so a collapse
// (or a fall-through to the buffered `put`) is observable.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StreamingStorageState {
    calls: Vec<&'static str>,
    folder: String,
    key: String,
    content_type: String,
    body_chunks: Vec<Vec<u8>>,
}

struct RecordingStreamingStorage {
    state: Arc<Mutex<StreamingStorageState>>,
}

impl RecordingStreamingStorage {
    fn new() -> (Self, Arc<Mutex<StreamingStorageState>>) {
        let state = Arc::new(Mutex::new(StreamingStorageState::default()));
        (
            Self {
                state: state.clone(),
            },
            state,
        )
    }
}

#[async_trait]
impl StorageService for RecordingStreamingStorage {
    /// Buffered `put` — if the streaming op ever collapsed and fell through to
    /// the buffered path, the round-trip test's `calls` assertion
    /// (`["put_streaming"]`) would catch it.
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), StorageError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push("put");
        s.folder = folder.to_string();
        s.key = key.to_string();
        s.content_type = content_type.to_string();
        // One buffered blob — deliberately recorded as a single chunk so a
        // buffered path is visibly distinct from the multi-chunk stream.
        s.body_chunks = vec![data.to_vec()];
        Ok(())
    }

    async fn put_streaming(
        &self,
        folder: &str,
        key: &str,
        mut data: InputStream,
        content_type: &str,
    ) -> Result<(), StorageError> {
        {
            let mut s = self.state.lock().unwrap();
            s.calls.push("put_streaming");
            s.folder = folder.to_string();
            s.key = key.to_string();
            s.content_type = content_type.to_string();
        }
        // Drain the body stream frame-by-frame — each chunk is stored
        // separately so the test can assert frame boundaries were preserved.
        while let Some(chunk) = data.next().await {
            self.state.lock().unwrap().body_chunks.push(chunk);
        }
        Ok(())
    }

    async fn get(&self, _folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        Ok((
            vec![],
            ObjectInfo {
                key: key.to_string(),
                size: 0,
                content_type: "application/octet-stream".to_string(),
                last_modified: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
            },
        ))
    }
    async fn delete(&self, _folder: &str, _key: &str) -> Result<(), StorageError> {
        Ok(())
    }
    async fn list(&self, _folder: &str, _opts: &ListOptions) -> Result<ObjectList, StorageError> {
        Ok(ObjectList {
            objects: vec![],
            total_count: 0,
        })
    }
    async fn create_folder(&self, _name: &str, _public: bool) -> Result<(), StorageError> {
        Ok(())
    }
    async fn delete_folder(&self, _name: &str) -> Result<(), StorageError> {
        Ok(())
    }
    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// 1. Non-collapsing round-trip through the macro-generated block dispatch.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_streaming_dispatch_streams_body_chunks_verbatim_to_put_streaming() {
    let (svc, state) = RecordingStreamingStorage::new();
    let block = StorageBlock::new(Arc::new(svc));

    let body_chunks = vec![
        b"chunk-a".to_vec(),
        b"chunk-b".to_vec(),
        b"chunk-c".to_vec(),
    ];
    let input = put_streaming_input("uploads", "big.bin", "image/png", &body_chunks);

    // Drive the real, macro-generated `Block::handle` — this exercises the
    // additive stream-ingress branch (raw InputStream, no collect_to_bytes).
    let out = block
        .handle(
            &RecordingCtx::allow(),
            msg(ServiceOp::STORAGE_PUT_STREAMING),
            input,
        )
        .await;

    // Success ack: an empty Response terminal.
    let buf = out
        .collect_buffered()
        .await
        .expect("streaming put must succeed with an ack");
    assert!(buf.body.is_empty(), "put ack body must be empty");

    let s = state.lock().unwrap();
    // The STREAMING method ran — never the buffered `put`.
    assert_eq!(
        s.calls,
        vec!["put_streaming"],
        "the streaming op must dispatch to put_streaming, never collapse to the buffered put"
    );
    // Header frame decoded correctly.
    assert_eq!(s.folder, "uploads");
    assert_eq!(s.key, "big.bin");
    assert_eq!(s.content_type, "image/png");
    // Body arrived as 3 DISTINCT frames — a collapsed/buffered path would have
    // handed a single concatenated blob (len == 1).
    assert_eq!(
        s.body_chunks, body_chunks,
        "body must stream as verbatim per-frame chunks, not a single buffered blob"
    );
}

/// The buffered `storage.put` path through the SAME block is unchanged — the
/// additive stream-ingress branch only intercepts the streaming op.
#[tokio::test]
async fn buffered_put_dispatch_still_routes_to_buffered_put() {
    let (svc, state) = RecordingStreamingStorage::new();
    let block = StorageBlock::new(Arc::new(svc));

    let body = codec::encode(&wire::storage::PutRequest {
        folder: "uploads".into(),
        key: "small.bin".into(),
        data: b"hello".to_vec(),
        content_type: "text/plain".into(),
    })
    .expect("encode PutRequest");

    let out = block
        .handle(
            &RecordingCtx::allow(),
            msg(ServiceOp::STORAGE_PUT),
            InputStream::from_bytes(body),
        )
        .await;
    out.collect_buffered()
        .await
        .expect("buffered put must still succeed");

    let s = state.lock().unwrap();
    assert_eq!(
        s.calls,
        vec!["put"],
        "the buffered op must still route to the buffered put"
    );
    assert_eq!(s.body_chunks, vec![b"hello".to_vec()]);
}

// ---------------------------------------------------------------------------
// 2. WRAP-authorization parity + deny.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_streaming_requests_identical_write_grant_to_buffered_put() {
    let (svc, _state) = RecordingStreamingStorage::new();

    // Buffered `storage.put` grant tuple, via the shared handler.
    let ctx_buffered = RecordingCtx::allow();
    let put_body = codec::encode(&wire::storage::PutRequest {
        folder: "uploads".into(),
        key: "big.bin".into(),
        data: b"x".to_vec(),
        content_type: "application/octet-stream".into(),
    })
    .expect("encode PutRequest");
    let _ = wafer_core::interfaces::storage::handler::handle_message(
        &svc,
        &ctx_buffered,
        &msg(ServiceOp::STORAGE_PUT),
        &put_body,
    )
    .await
    .collect_buffered()
    .await;

    // Streaming `storage.put_streaming` grant tuple, via the streaming handler.
    let ctx_streaming = RecordingCtx::allow();
    let input = put_streaming_input(
        "uploads",
        "big.bin",
        "application/octet-stream",
        &[b"x".to_vec()],
    );
    let _ = wafer_core::interfaces::storage::handler::handle_put_streaming(
        &svc,
        &ctx_streaming,
        &msg(ServiceOp::STORAGE_PUT_STREAMING),
        input,
    )
    .await
    .collect_buffered()
    .await;

    assert_eq!(
        ctx_streaming.seen(),
        ctx_buffered.seen(),
        "streaming upload must request the IDENTICAL WRAP grant tuple as the buffered upload"
    );
    // And concretely: a WRITE (is_write=true) of `{folder}/{key}` on Storage.
    assert_eq!(
        ctx_streaming.seen(),
        vec![("uploads/big.bin".to_string(), ResourceType::Storage, true)],
    );
}

#[tokio::test]
async fn put_streaming_denied_without_the_write_grant() {
    let (svc, state) = RecordingStreamingStorage::new();

    let ctx = RecordingCtx::deny();
    let input = put_streaming_input(
        "uploads",
        "big.bin",
        "application/octet-stream",
        &[b"secret".to_vec()],
    );
    let out = wafer_core::interfaces::storage::handler::handle_put_streaming(
        &svc,
        &ctx,
        &msg(ServiceOp::STORAGE_PUT_STREAMING),
        input,
    )
    .await;
    expect_permission_denied(out).await;

    // A denied streaming upload must never reach the service — no body frame
    // is consumed or written.
    assert!(
        state.lock().unwrap().calls.is_empty(),
        "a denied streaming put must never reach the service; calls = {:?}",
        state.lock().unwrap().calls
    );
    // The denial consulted exactly the buffered op's write grant.
    assert_eq!(
        ctx.seen(),
        vec![("uploads/big.bin".to_string(), ResourceType::Storage, true)],
    );
}

// ---------------------------------------------------------------------------
// 3. Full client round-trip: clients::storage::put_stream frames the header +
//    streams the body through `call_block` into the macro-generated block and
//    the backend's `put_streaming`.
// ---------------------------------------------------------------------------

/// `Context` that routes `call_block` into a wrapped `StorageBlock` (so the
/// client's request flows through the real macro-generated dispatch) and
/// records/allows the WRAP grant the handler consults.
struct BlockRoutingCtx {
    block: Arc<StorageBlock>,
    seen: Mutex<Vec<(String, ResourceType, bool)>>,
}

#[wafer_block::wafer_async_trait]
impl Context for BlockRoutingCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        msg: Message,
        input: InputStream,
    ) -> OutputStream {
        self.block.handle(self, msg, input).await
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("not exercised by this test")
    }

    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: ResourceType,
        is_write: bool,
    ) -> Result<(), WaferError> {
        self.seen
            .lock()
            .unwrap()
            .push((resource.to_string(), resource_type, is_write));
        Ok(())
    }
}

#[tokio::test]
async fn client_put_stream_round_trips_header_and_body_into_put_streaming() {
    let (svc, state) = RecordingStreamingStorage::new();
    let ctx = BlockRoutingCtx {
        block: Arc::new(StorageBlock::new(Arc::new(svc))),
        seen: Mutex::new(Vec::new()),
    };

    let body_chunks = vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()];
    let body = InputStream::from_stream(futures::stream::iter(body_chunks.clone()));

    wafer_core::clients::storage::put_stream(&ctx, "uploads", "media.bin", "video/mp4", body)
        .await
        .expect("client put_stream must succeed");

    let s = state.lock().unwrap();
    assert_eq!(s.calls, vec!["put_streaming"]);
    assert_eq!(s.folder, "uploads");
    assert_eq!(s.key, "media.bin");
    assert_eq!(s.content_type, "video/mp4");
    // The client framed the header separately; the backend saw the body as its
    // own verbatim per-frame chunks, never the header and never a single blob.
    assert_eq!(s.body_chunks, body_chunks);

    // The client stamped — and the handler consulted — the SAME write grant as
    // the buffered put: a WRITE of `{folder}/{key}` on Storage.
    assert_eq!(
        *ctx.seen.lock().unwrap(),
        vec![("uploads/media.bin".to_string(), ResourceType::Storage, true)],
    );
}

/// A streaming-put request whose stream ends before the header frame is a
/// malformed request — rejected with `InvalidArgument`, and the service is
/// never touched.
#[tokio::test]
async fn put_streaming_missing_header_frame_is_invalid_argument() {
    let (svc, state) = RecordingStreamingStorage::new();

    let out = wafer_core::interfaces::storage::handler::handle_put_streaming(
        &svc,
        &RecordingCtx::allow(),
        &msg(ServiceOp::STORAGE_PUT_STREAMING),
        InputStream::empty(),
    )
    .await;

    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(e.code, ErrorCode::InvalidArgument);
            assert!(
                e.message.contains("storage.put_streaming"),
                "error must name the op, got: {}",
                e.message
            );
        }
        other => panic!("expected an InvalidArgument error terminal, got {other:?}"),
    }
    assert!(
        state.lock().unwrap().calls.is_empty(),
        "a headerless streaming put must never reach the service"
    );
}
