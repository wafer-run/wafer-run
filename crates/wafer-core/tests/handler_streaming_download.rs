//! Streaming-download dispatch tests for the wired `STORAGE_GET_STREAMING` and
//! `NETWORK_DO_REQUEST_STREAMING` ops.
//!
//! Two properties are pinned per service:
//!
//! 1. **Round-trip through the dispatch path** (op string → handler → the
//!    service's `get_streaming` / `do_request_streaming` → `OutputStream`
//!    frames): the handler forwards the service's body chunks **verbatim** as
//!    ordered frames — the typed header frame first, then each body chunk as
//!    its own `Chunk` event — and never collapses them into a single buffered
//!    blob (which is exactly what the streaming path exists to avoid). A
//!    recording fake proves the streaming service method ran, not the buffered
//!    one.
//!
//! 2. **WRAP-authorization parity** (the security focus of this change): a
//!    recording `Context` captures the exact `(resource, resource_type,
//!    is_write)` tuple each op hands to `check_resource_access`, and the test
//!    asserts the streaming op requests the **identical** grant tuple as its
//!    buffered counterpart — and is denied identically when that grant is
//!    absent. So the streaming download can never be reached with a weaker (or
//!    different) gate than the buffered download.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use futures::StreamExt;
use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    stream::StreamEvent,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::ResourceType,
    wire, ErrorCode, Message, WaferError,
};
use wafer_core::interfaces::{
    network::service::{NetworkError, NetworkService, Request, Response, ResponseHead},
    storage::service::{
        FolderInfo, ListOptions, ObjectInfo, ObjectList, StorageError, StorageService,
    },
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

type Calls = Arc<Mutex<Vec<&'static str>>>;

fn new_calls() -> Calls {
    Arc::new(Mutex::new(Vec::new()))
}

/// A bare `Message` carrying only `kind` — no WRAP meta. The recording
/// `Context` (not any message meta) is what gates the call.
fn msg(kind: &str) -> Message {
    Message::new(kind)
}

fn object_info(key: &str, size: i64) -> ObjectInfo {
    ObjectInfo {
        key: key.to_string(),
        size,
        content_type: "application/octet-stream".to_string(),
        last_modified: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
    }
}

/// Drain an `OutputStream` into its raw event sequence — deliberately NOT
/// `collect_buffered`, which would concatenate every `Chunk` and destroy the
/// frame boundaries this test needs to observe.
async fn events(out: OutputStream) -> Vec<StreamEvent> {
    out.collect().await
}

/// Extract the `Chunk` payloads (frame boundaries preserved) from an event
/// sequence.
fn chunks(events: &[StreamEvent]) -> Vec<Vec<u8>> {
    events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Chunk(b) => Some(b.clone()),
            _ => None,
        })
        .collect()
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
        unimplemented!("not exercised by decode_and_authorize / check_resource_access")
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize / check_resource_access")
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
// Storage — a fake whose `get_streaming` override emits several distinct body
// chunks, so a collapse would be observable (chunks.len() would drop to 1).
// ---------------------------------------------------------------------------

struct StreamingStorage {
    calls: Calls,
    body_chunks: Vec<Vec<u8>>,
}

impl StreamingStorage {
    fn new(calls: Calls, body_chunks: Vec<Vec<u8>>) -> Self {
        Self { calls, body_chunks }
    }
    fn record(&self, op: &'static str) {
        self.calls.lock().unwrap().push(op);
    }
    fn total_len(&self) -> i64 {
        self.body_chunks.iter().map(|c| c.len()).sum::<usize>() as i64
    }
}

#[async_trait]
impl StorageService for StreamingStorage {
    async fn put(
        &self,
        _folder: &str,
        _key: &str,
        _data: &[u8],
        _content_type: &str,
    ) -> Result<(), StorageError> {
        self.record("put");
        Ok(())
    }

    /// Buffered `get` — if the streaming op ever fell back to this, the
    /// round-trip test's `calls` assertion (`["get_streaming"]`) would fail,
    /// catching a regression that quietly reverts to buffering.
    async fn get(&self, _folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        self.record("get");
        Ok((
            self.body_chunks.concat(),
            object_info(key, self.total_len()),
        ))
    }

    async fn get_streaming(
        &self,
        _folder: &str,
        key: &str,
    ) -> Result<(OutputStream, ObjectInfo), StorageError> {
        self.record("get_streaming");
        let info = object_info(key, self.total_len());
        let body_chunks = self.body_chunks.clone();
        let stream = OutputStream::from_producer(move |sink, _cancel| async move {
            for c in body_chunks {
                if sink.send_chunk(c).await.is_err() {
                    return;
                }
            }
            let _ = sink.complete(vec![]).await;
        });
        Ok((stream, info))
    }

    async fn delete(&self, _folder: &str, _key: &str) -> Result<(), StorageError> {
        self.record("delete");
        Ok(())
    }
    async fn list(&self, _folder: &str, _opts: &ListOptions) -> Result<ObjectList, StorageError> {
        self.record("list");
        Ok(ObjectList {
            objects: vec![],
            total_count: 0,
            next_cursor: None,
        })
    }
    async fn create_folder(&self, _name: &str, _public: bool) -> Result<(), StorageError> {
        self.record("create_folder");
        Ok(())
    }
    async fn delete_folder(&self, _name: &str) -> Result<(), StorageError> {
        self.record("delete_folder");
        Ok(())
    }
    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        self.record("list_folders");
        Ok(vec![])
    }
}

fn storage_get_body() -> Vec<u8> {
    codec::encode(&wire::storage::GetRequest {
        folder: "uploads".into(),
        key: "big.bin".into(),
    })
    .expect("encode GetRequest")
}

#[tokio::test]
async fn storage_get_streaming_forwards_body_chunks_verbatim_in_order() {
    let calls = new_calls();
    let svc = StreamingStorage::new(
        calls.clone(),
        vec![
            b"chunk-a".to_vec(),
            b"chunk-b".to_vec(),
            b"chunk-c".to_vec(),
        ],
    );

    let out = wafer_core::interfaces::storage::handler::handle_message(
        &svc,
        &RecordingCtx::allow(),
        &msg(ServiceOp::STORAGE_GET_STREAMING),
        &storage_get_body(),
    )
    .await;

    let evts = events(out).await;
    assert!(
        matches!(evts.last(), Some(StreamEvent::Complete { .. })),
        "stream must terminate with Complete, got: {evts:?}"
    );

    let frames = chunks(&evts);
    // Header frame + 3 DISTINCT body frames. A buffered-then-reframed path
    // would collapse the body to a single chunk (frames.len() == 2).
    assert_eq!(
        frames.len(),
        4,
        "expected an ObjectInfo header frame + 3 verbatim body frames (no collapse), got {} frames",
        frames.len()
    );

    let info: wire::storage::ObjectInfo =
        codec::decode(&frames[0]).expect("first frame is the ObjectInfo header");
    assert_eq!(info.key, "big.bin");
    assert_eq!(
        info.size,
        (b"chunk-a".len() + b"chunk-b".len() + b"chunk-c".len()) as i64
    );

    assert_eq!(&frames[1], b"chunk-a");
    assert_eq!(&frames[2], b"chunk-b");
    assert_eq!(&frames[3], b"chunk-c");

    // The STREAMING service method ran — not the buffered `get`.
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["get_streaming"],
        "the streaming op must dispatch to get_streaming, never the buffered get"
    );
}

#[tokio::test]
async fn storage_get_streaming_requests_identical_grant_to_buffered_get() {
    let body = storage_get_body();
    let svc = StreamingStorage::new(new_calls(), vec![b"x".to_vec()]);

    // Buffered `storage.get` grant tuple.
    let ctx_buffered = RecordingCtx::allow();
    let _ = events(
        wafer_core::interfaces::storage::handler::handle_message(
            &svc,
            &ctx_buffered,
            &msg(ServiceOp::STORAGE_GET),
            &body,
        )
        .await,
    )
    .await;

    // Streaming `storage.get_streaming` grant tuple.
    let ctx_streaming = RecordingCtx::allow();
    let _ = events(
        wafer_core::interfaces::storage::handler::handle_message(
            &svc,
            &ctx_streaming,
            &msg(ServiceOp::STORAGE_GET_STREAMING),
            &body,
        )
        .await,
    )
    .await;

    assert_eq!(
        ctx_streaming.seen(),
        ctx_buffered.seen(),
        "streaming download must request the IDENTICAL WRAP grant tuple as the buffered download"
    );
    // And concretely: a read (is_write=false) of `{folder}/{key}` on Storage.
    assert_eq!(
        ctx_streaming.seen(),
        vec![("uploads/big.bin".to_string(), ResourceType::Storage, false)],
    );
}

#[tokio::test]
async fn storage_get_streaming_denied_without_the_grant() {
    let calls = new_calls();
    let svc = StreamingStorage::new(calls.clone(), vec![b"secret".to_vec()]);

    let ctx = RecordingCtx::deny();
    let out = wafer_core::interfaces::storage::handler::handle_message(
        &svc,
        &ctx,
        &msg(ServiceOp::STORAGE_GET_STREAMING),
        &storage_get_body(),
    )
    .await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "a denied streaming get must never reach the service; calls = {:?}",
        calls.lock().unwrap()
    );
    // The denial consulted exactly the buffered op's grant — read of the object.
    assert_eq!(
        ctx.seen(),
        vec![("uploads/big.bin".to_string(), ResourceType::Storage, false)],
    );
}

// ---------------------------------------------------------------------------
// Network — a fake whose `do_request_streaming` override emits several distinct
// body chunks.
// ---------------------------------------------------------------------------

struct StreamingNetwork {
    calls: Calls,
    status: u16,
    body_chunks: Vec<Vec<u8>>,
}

impl StreamingNetwork {
    fn new(calls: Calls, status: u16, body_chunks: Vec<Vec<u8>>) -> Self {
        Self {
            calls,
            status,
            body_chunks,
        }
    }
    fn record(&self, op: &'static str) {
        self.calls.lock().unwrap().push(op);
    }
}

#[async_trait]
impl NetworkService for StreamingNetwork {
    /// Buffered `do_request` — the round-trip test asserts this never runs.
    async fn do_request(&self, _req: &Request) -> Result<Response, NetworkError> {
        self.record("do_request");
        Ok(Response {
            status_code: self.status,
            headers: HashMap::new(),
            body: self.body_chunks.concat(),
        })
    }

    async fn do_request_streaming(
        &self,
        _req: &Request,
    ) -> Result<(ResponseHead, OutputStream), NetworkError> {
        self.record("do_request_streaming");
        let head = ResponseHead {
            status_code: self.status,
            headers: HashMap::new(),
        };
        let body_chunks = self.body_chunks.clone();
        let stream = OutputStream::from_producer(move |sink, _cancel| async move {
            for c in body_chunks {
                if sink.send_chunk(c).await.is_err() {
                    return;
                }
            }
            let _ = sink.complete(vec![]).await;
        });
        Ok((head, stream))
    }
}

fn network_do_body() -> Vec<u8> {
    codec::encode(&wire::network::Request {
        method: "GET".into(),
        url: "https://example.test/media".into(),
        headers: HashMap::new(),
        body: None,
    })
    .expect("encode network Request")
}

#[tokio::test]
async fn network_do_streaming_forwards_body_chunks_verbatim_in_order() {
    let calls = new_calls();
    let svc = StreamingNetwork::new(
        calls.clone(),
        200,
        vec![
            b"chunk-a".to_vec(),
            b"chunk-b".to_vec(),
            b"chunk-c".to_vec(),
        ],
    );

    let out = wafer_core::interfaces::network::handler::handle_message(
        &svc,
        &RecordingCtx::allow(),
        &msg(ServiceOp::NETWORK_DO_REQUEST_STREAMING),
        &network_do_body(),
    )
    .await;

    let evts = events(out).await;
    assert!(
        matches!(evts.last(), Some(StreamEvent::Complete { .. })),
        "stream must terminate with Complete, got: {evts:?}"
    );

    let frames = chunks(&evts);
    assert_eq!(
        frames.len(),
        4,
        "expected a ResponseHeader header frame + 3 verbatim body frames (no collapse), got {} frames",
        frames.len()
    );

    let header: wire::network::ResponseHeader =
        codec::decode(&frames[0]).expect("first frame is the ResponseHeader");
    assert_eq!(header.status_code, 200);

    assert_eq!(&frames[1], b"chunk-a");
    assert_eq!(&frames[2], b"chunk-b");
    assert_eq!(&frames[3], b"chunk-c");

    assert_eq!(
        *calls.lock().unwrap(),
        vec!["do_request_streaming"],
        "the streaming op must dispatch to do_request_streaming, never the buffered do_request"
    );
}

#[tokio::test]
async fn network_do_streaming_requests_identical_grant_to_buffered_do() {
    let body = network_do_body();
    let svc = StreamingNetwork::new(new_calls(), 200, vec![b"x".to_vec()]);

    let ctx_buffered = RecordingCtx::allow();
    let _ = events(
        wafer_core::interfaces::network::handler::handle_message(
            &svc,
            &ctx_buffered,
            &msg(ServiceOp::NETWORK_DO_REQUEST),
            &body,
        )
        .await,
    )
    .await;

    let ctx_streaming = RecordingCtx::allow();
    let _ = events(
        wafer_core::interfaces::network::handler::handle_message(
            &svc,
            &ctx_streaming,
            &msg(ServiceOp::NETWORK_DO_REQUEST_STREAMING),
            &body,
        )
        .await,
    )
    .await;

    assert_eq!(
        ctx_streaming.seen(),
        ctx_buffered.seen(),
        "streaming request must request the IDENTICAL WRAP grant tuple as the buffered request"
    );
    // And concretely: a read (is_write=false) of the target URL on Network.
    assert_eq!(
        ctx_streaming.seen(),
        vec![(
            "https://example.test/media".to_string(),
            ResourceType::Network,
            false
        )],
    );
}

#[tokio::test]
async fn network_do_streaming_denied_without_the_grant() {
    let calls = new_calls();
    let svc = StreamingNetwork::new(calls.clone(), 200, vec![b"secret".to_vec()]);

    let ctx = RecordingCtx::deny();
    let out = wafer_core::interfaces::network::handler::handle_message(
        &svc,
        &ctx,
        &msg(ServiceOp::NETWORK_DO_REQUEST_STREAMING),
        &network_do_body(),
    )
    .await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "a denied streaming request must never reach the service; calls = {:?}",
        calls.lock().unwrap()
    );
    assert_eq!(
        ctx.seen(),
        vec![(
            "https://example.test/media".to_string(),
            ResourceType::Network,
            false
        )],
    );
}
