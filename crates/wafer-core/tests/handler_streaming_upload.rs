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
    types::{ResourceAccess, ResourceType},
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
    let mut frames = vec![Ok(header)];
    frames.extend(body_chunks.iter().cloned().map(Ok));
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
    seen: Mutex<Vec<(String, ResourceType, ResourceAccess)>>,
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
    fn seen(&self) -> Vec<(String, ResourceType, ResourceAccess)> {
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

    // The calling block: the handler scopes the plain folder `uploads` into
    // `test/caller/uploads` before it authorizes and before the service runs.
    fn caller_id(&self) -> Option<&str> {
        Some("test/caller")
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
        access: ResourceAccess,
    ) -> Result<(), WaferError> {
        self.seen
            .lock()
            .unwrap()
            .push((resource.to_string(), resource_type, access));
        if self.allow {
            Ok(())
        } else {
            Err(WaferError::new(
                ErrorCode::PermissionDenied,
                "denied by test ctx",
            ))
        }
    }

    // Same policy as `check_resource_access`, without recording a check.
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _access: ResourceAccess,
    ) -> bool {
        self.allow
    }
}

// ---------------------------------------------------------------------------
// Recording storage fake — its `put_streaming` override drains the body
// `InputStream` chunk-by-chunk and records each distinct frame, so a collapse
// (or a fall-through to the buffered `put`) is observable.
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct StreamingStorageState {
    calls: Vec<&'static str>,
    folder: String,
    key: String,
    content_type: String,
    body_chunks: Vec<Vec<u8>>,
}

/// Snapshot the recorded state into an owned copy — releases the lock
/// immediately so assertions never hold the guard.
fn snapshot(state: &Arc<Mutex<StreamingStorageState>>) -> StreamingStorageState {
    state.lock().unwrap().clone()
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
        {
            let mut s = self.state.lock().unwrap();
            s.calls.push("put");
            s.folder = folder.to_string();
            s.key = key.to_string();
            s.content_type = content_type.to_string();
            // One buffered blob — deliberately recorded as a single chunk so a
            // buffered path is visibly distinct from the multi-chunk stream.
            s.body_chunks = vec![data.to_vec()];
        }
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
            let chunk = chunk.map_err(StorageError::Body)?;
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
            next_cursor: None,
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

    let s = snapshot(&state);
    // The STREAMING method ran — never the buffered `put`.
    assert_eq!(
        s.calls,
        vec!["put_streaming"],
        "the streaming op must dispatch to put_streaming, never collapse to the buffered put"
    );
    // Header frame decoded correctly.
    assert_eq!(s.folder, "test/caller/uploads");
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

    let s = snapshot(&state);
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
    // And concretely: a WRITE (is_write=true) of the caller-scoped `{folder}/{key}` on Storage.
    assert_eq!(
        ctx_streaming.seen(),
        vec![(
            "test/caller/uploads/big.bin".to_string(),
            ResourceType::Storage,
            ResourceAccess::Write
        )],
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
        vec![(
            "test/caller/uploads/big.bin".to_string(),
            ResourceType::Storage,
            ResourceAccess::Write
        )],
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
    seen: Mutex<Vec<(String, ResourceType, ResourceAccess)>>,
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

    // The calling block: the handler scopes the plain folder `uploads` into
    // `test/caller/uploads` before it authorizes and before the service runs.
    fn caller_id(&self) -> Option<&str> {
        Some("test/caller")
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
        access: ResourceAccess,
    ) -> Result<(), WaferError> {
        self.seen
            .lock()
            .unwrap()
            .push((resource.to_string(), resource_type, access));
        Ok(())
    }

    // Same policy as `check_resource_access`, without recording a check.
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _access: ResourceAccess,
    ) -> bool {
        true
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
    let body = InputStream::from_stream(futures::stream::iter(
        body_chunks.clone().into_iter().map(Ok),
    ));

    wafer_core::clients::storage::put_stream(&ctx, "uploads", "media.bin", "video/mp4", body)
        .await
        .expect("client put_stream must succeed");

    let s = snapshot(&state);
    assert_eq!(s.calls, vec!["put_streaming"]);
    assert_eq!(s.folder, "test/caller/uploads");
    assert_eq!(s.key, "media.bin");
    assert_eq!(s.content_type, "video/mp4");
    // The client framed the header separately; the backend saw the body as its
    // own verbatim per-frame chunks, never the header and never a single blob.
    assert_eq!(s.body_chunks, body_chunks);

    // The client stamped — and the handler consulted — the SAME write grant as
    // the buffered put: a WRITE of the caller-scoped `{folder}/{key}` on Storage.
    assert_eq!(
        *ctx.seen.lock().unwrap(),
        vec![(
            "test/caller/uploads/media.bin".to_string(),
            ResourceType::Storage,
            ResourceAccess::Write
        )],
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

// ---------------------------------------------------------------------------
// 4. A body that fails is never stored.
// ---------------------------------------------------------------------------

/// The error a transport puts on a body that did not arrive whole.
fn body_timed_out() -> WaferError {
    WaferError::new(ErrorCode::DeadlineExceeded, "request body timed out")
}

/// A backend that keeps `StorageService::put_streaming`'s trait default
/// (collect, then `put`) and records every `put` it receives.
#[derive(Default)]
struct BufferedOnlyStorage {
    puts: Mutex<Vec<Vec<u8>>>,
}

#[async_trait]
impl StorageService for BufferedOnlyStorage {
    async fn put(
        &self,
        _folder: &str,
        _key: &str,
        data: &[u8],
        _content_type: &str,
    ) -> Result<(), StorageError> {
        self.puts.lock().unwrap().push(data.to_vec());
        Ok(())
    }
    async fn get(&self, _folder: &str, _key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        Err(StorageError::NotFound)
    }
    async fn delete(&self, _folder: &str, _key: &str) -> Result<(), StorageError> {
        Ok(())
    }
    async fn list(&self, _folder: &str, _opts: &ListOptions) -> Result<ObjectList, StorageError> {
        Ok(ObjectList::default())
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

/// `input` with its last frame replaced by a failure.
fn failing_after(mut frames: Vec<Vec<u8>>) -> InputStream {
    frames.pop();
    let mut items: Vec<Result<Vec<u8>, WaferError>> = frames.into_iter().map(Ok).collect();
    items.push(Err(body_timed_out()));
    InputStream::from_stream(futures::stream::iter(items))
}

fn header_frame(folder: &str, key: &str) -> Vec<u8> {
    codec::encode(&wire::storage::PutStreamingHeader {
        folder: folder.to_string(),
        key: key.to_string(),
        content_type: "text/plain".to_string(),
    })
    .expect("encode PutStreamingHeader")
}

async fn expect_body_error(out: OutputStream) {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => assert_eq!(e, body_timed_out()),
        other => panic!("expected the body's own error, got {other:?}"),
    }
}

/// Through the real block dispatch into a backend on the trait default: a
/// body that fails after its first chunk returns the body's error and the
/// backend's `put` is never called with the prefix.
#[tokio::test]
async fn put_streaming_with_a_failing_body_stores_nothing_on_the_trait_default() {
    let svc = Arc::new(BufferedOnlyStorage::default());
    let block = StorageBlock::new(svc.clone());

    let input = failing_after(vec![
        header_frame("uploads", "k"),
        b"first half".to_vec(),
        b"never arrives".to_vec(),
    ]);
    let out = block
        .handle(
            &RecordingCtx::allow(),
            msg(ServiceOp::STORAGE_PUT_STREAMING),
            input,
        )
        .await;

    expect_body_error(out).await;
    assert!(
        svc.puts.lock().unwrap().is_empty(),
        "a truncated body must not be stored"
    );
}

/// A backend that streams (`put_streaming` overridden) gets the failure and
/// its error is what the caller sees.
#[tokio::test]
async fn put_streaming_with_a_failing_body_is_an_error_on_a_streaming_backend() {
    let (svc, state) = RecordingStreamingStorage::new();
    let block = StorageBlock::new(Arc::new(svc));

    let input = failing_after(vec![
        header_frame("uploads", "k"),
        b"first half".to_vec(),
        b"never arrives".to_vec(),
    ]);
    let out = block
        .handle(
            &RecordingCtx::allow(),
            msg(ServiceOp::STORAGE_PUT_STREAMING),
            input,
        )
        .await;

    expect_body_error(out).await;
    assert_eq!(snapshot(&state).body_chunks, vec![b"first half".to_vec()]);
}

/// A stream that fails before its header frame never reaches the service.
#[tokio::test]
async fn put_streaming_failing_before_the_header_never_reaches_the_service() {
    let (svc, state) = RecordingStreamingStorage::new();
    let out = wafer_core::interfaces::storage::handler::handle_put_streaming(
        &svc,
        &RecordingCtx::allow(),
        &msg(ServiceOp::STORAGE_PUT_STREAMING),
        failing_after(vec![header_frame("uploads", "k")]),
    )
    .await;

    expect_body_error(out).await;
    assert!(state.lock().unwrap().calls.is_empty());
}

/// `clients::storage::put_stream` with a failing body returns the body's
/// error, and nothing is stored.
#[tokio::test]
async fn client_put_stream_with_a_failing_body_returns_its_error() {
    let svc = Arc::new(BufferedOnlyStorage::default());
    let ctx = BlockRoutingCtx {
        block: Arc::new(StorageBlock::new(svc.clone())),
        seen: Mutex::new(Vec::new()),
    };

    let body = failing_after(vec![b"first half".to_vec(), b"never arrives".to_vec()]);
    let err = wafer_core::clients::storage::put_stream(&ctx, "uploads", "k", "text/plain", body)
        .await
        .expect_err("a failed body is not a successful upload");
    assert_eq!(err, body_timed_out());
    assert!(svc.puts.lock().unwrap().is_empty());
}

/// The buffered ops collect the request body in the `service_block!` macro:
/// a body that fails there is refused with its own error before the handler
/// decodes a prefix of it.
#[tokio::test]
async fn buffered_put_with_a_failing_body_is_refused_before_the_handler() {
    let svc = Arc::new(BufferedOnlyStorage::default());
    let block = StorageBlock::new(svc.clone());

    let request = codec::encode(&wire::storage::PutRequest {
        folder: "uploads".into(),
        key: "k".into(),
        data: b"whole".to_vec(),
        content_type: "text/plain".into(),
    })
    .expect("encode PutRequest");
    let input = failing_after(vec![request, Vec::new()]);
    let out = block
        .handle(&RecordingCtx::allow(), msg(ServiceOp::STORAGE_PUT), input)
        .await;

    expect_body_error(out).await;
    assert!(svc.puts.lock().unwrap().is_empty());
}
