//! Task 6 exploit-shape tests — the storage handler now authorizes every
//! op arm through `decode_and_authorize` (host-side `ctx.check_resource_access`)
//! instead of the caller-suppliable `wrap.resource` message meta.
//!
//! These tests reconstruct the meta-omission shape (WRAP metas absent on the
//! message) and assert the *ctx*, not the meta, is what gates the call: a
//! denying `Context` must produce `PermissionDenied` for `storage.put`,
//! `storage.get`, and a foreign-folder `storage.list` — and, via a recording
//! fake `StorageService`, that the underlying service method never actually
//! ran. A granting `Context` must let the same requests through and reach
//! the service.

use std::sync::{Arc, Mutex};

use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::ResourceType,
    wire, ErrorCode, Message, WaferError,
};

// ---------------------------------------------------------------------------
// Recording fake StorageService — records every op invoked so tests can
// assert a denied request never reached the service, not just that the
// handler returned the right error.
// ---------------------------------------------------------------------------

mod storage_fakes {
    use async_trait::async_trait;
    use wafer_core::interfaces::storage::service::{
        FolderInfo, ListOptions, ObjectInfo, ObjectList, StorageError, StorageService,
    };

    use super::Calls;

    pub struct RecordingStorage {
        pub calls: Calls,
    }

    impl RecordingStorage {
        pub fn new(calls: Calls) -> Self {
            Self { calls }
        }

        fn record(&self, op: &'static str) {
            self.calls.lock().unwrap().push(op);
        }
    }

    #[async_trait]
    impl StorageService for RecordingStorage {
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
        async fn get(
            &self,
            _folder: &str,
            key: &str,
        ) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
            self.record("get");
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
            self.record("delete");
            Ok(())
        }
        async fn list(
            &self,
            _folder: &str,
            _opts: &ListOptions,
        ) -> Result<ObjectList, StorageError> {
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
}

/// Shared call log, checked via `Arc::clone` from the test after the handler
/// call returns.
type Calls = Arc<Mutex<Vec<&'static str>>>;

fn new_calls() -> Calls {
    Arc::new(Mutex::new(Vec::new()))
}

// ---------------------------------------------------------------------------
// Context fakes
// ---------------------------------------------------------------------------

/// `Context` stub that denies every resource-access check — models a caller
/// with no WRAP grant for anything, regardless of what (if any) meta the
/// message carries.
struct DenyCtx;

#[wafer_block::wafer_async_trait]
impl Context for DenyCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    // `check_resource_access` uses the trait's fail-closed default (deny).
}

/// `Context` stub that grants every resource-access check — models a caller
/// holding a valid WRAP grant for the resource it's requesting.
struct AllowCtx;

#[wafer_block::wafer_async_trait]
impl Context for AllowCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn check_resource_access(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _is_write: bool,
    ) -> Result<(), WaferError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A bare `Message` carrying only `kind` — no `wrap.resource` /
/// `wrap.access` / `wrap.resource_type` meta at all. This is the exact
/// "meta-omission" shape: a caller (or a compromised WASM guest bypassing
/// the client wrapper) that never stamps WRAP meta.
fn msg_without_wrap_meta(kind: &str) -> Message {
    Message::new(kind)
}

async fn expect_permission_denied(out: OutputStream) -> WaferError {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(
                e.code,
                ErrorCode::PermissionDenied,
                "expected PERMISSION_DENIED, got {:?}: {}",
                e.code,
                e.message
            );
            e
        }
        other => panic!("expected a PermissionDenied error terminal, got {other:?}"),
    }
}

async fn expect_invalid_argument(out: OutputStream) -> WaferError {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(
                e.code,
                ErrorCode::InvalidArgument,
                "expected INVALID_ARGUMENT, got {:?}: {}",
                e.code,
                e.message
            );
            e
        }
        other => panic!("expected an InvalidArgument error terminal, got {other:?}"),
    }
}

async fn expect_success(out: OutputStream) {
    if let Err(TerminalNotResponse::Error(e)) = out.collect_buffered().await {
        panic!("expected success, got error {:?}: {}", e.code, e.message);
    }
}

// ---------------------------------------------------------------------------
// DENY cases — meta absent, ctx denies. Assert PermissionDenied AND that the
// service op never ran.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = storage_fakes::RecordingStorage::new(calls.clone());
    let req = wire::storage::PutRequest {
        folder: "uploads".into(),
        key: "evil.png".into(),
        data: vec![1, 2, 3],
        content_type: "image/png".into(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::STORAGE_PUT);

    let out =
        wafer_core::interfaces::storage::handler::handle_message(&svc, &DenyCtx, &msg, &body).await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "put must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn get_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = storage_fakes::RecordingStorage::new(calls.clone());
    let req = wire::storage::GetRequest {
        folder: "uploads".into(),
        key: "secret.png".into(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::STORAGE_GET);

    let out =
        wafer_core::interfaces::storage::handler::handle_message(&svc, &DenyCtx, &msg, &body).await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "get must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn foreign_folder_list_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = storage_fakes::RecordingStorage::new(calls.clone());
    let req = wire::storage::ListRequest {
        folder: "my_org__other_block__secrets".into(),
        prefix: String::new(),
        limit: 10,
        offset: 0,
        cursor: None,
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::STORAGE_LIST);

    let out =
        wafer_core::interfaces::storage::handler::handle_message(&svc, &DenyCtx, &msg, &body).await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "list on a foreign folder must not run; calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn list_folders_denied_under_deny_ctx_never_reaches_service() {
    let calls = new_calls();
    let svc = storage_fakes::RecordingStorage::new(calls.clone());
    // list_folders carries no meaningful body — the handler authorizes
    // against the admin-only STORAGE_LIST_ALL_RESOURCE sentinel.
    let body = codec::encode(&serde_json::json!({})).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::STORAGE_LIST_FOLDERS);

    let out =
        wafer_core::interfaces::storage::handler::handle_message(&svc, &DenyCtx, &msg, &body).await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "list_folders must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );
}

// ---------------------------------------------------------------------------
// ALLOW case — granted ctx lets the request through to the service, for
// every op the DENY cases above cover.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_folders_allowed_under_admin_ctx() {
    let calls = new_calls();
    let svc = storage_fakes::RecordingStorage::new(calls.clone());
    // AllowCtx grants the sentinel check (models the admin block) → the
    // service's list_folders runs.
    let body = codec::encode(&serde_json::json!({})).unwrap();
    expect_success(
        wafer_core::interfaces::storage::handler::handle_message(
            &svc,
            &AllowCtx,
            &msg_without_wrap_meta(ServiceOp::STORAGE_LIST_FOLDERS),
            &body,
        )
        .await,
    )
    .await;

    assert_eq!(
        *calls.lock().unwrap(),
        vec!["list_folders"],
        "list_folders should reach the service under an allowing ctx"
    );
}

#[tokio::test]
async fn granted_ctx_allows_put_get_and_list() {
    let calls = new_calls();
    let svc = storage_fakes::RecordingStorage::new(calls.clone());

    let put_body = codec::encode(&wire::storage::PutRequest {
        folder: "uploads".into(),
        key: "img.png".into(),
        data: vec![1, 2, 3],
        content_type: "image/png".into(),
    })
    .unwrap();
    expect_success(
        wafer_core::interfaces::storage::handler::handle_message(
            &svc,
            &AllowCtx,
            &msg_without_wrap_meta(ServiceOp::STORAGE_PUT),
            &put_body,
        )
        .await,
    )
    .await;

    let get_body = codec::encode(&wire::storage::GetRequest {
        folder: "uploads".into(),
        key: "img.png".into(),
    })
    .unwrap();
    expect_success(
        wafer_core::interfaces::storage::handler::handle_message(
            &svc,
            &AllowCtx,
            &msg_without_wrap_meta(ServiceOp::STORAGE_GET),
            &get_body,
        )
        .await,
    )
    .await;

    let list_body = codec::encode(&wire::storage::ListRequest {
        folder: "uploads".into(),
        prefix: String::new(),
        limit: 10,
        offset: 0,
        cursor: None,
    })
    .unwrap();
    expect_success(
        wafer_core::interfaces::storage::handler::handle_message(
            &svc,
            &AllowCtx,
            &msg_without_wrap_meta(ServiceOp::STORAGE_LIST),
            &list_body,
        )
        .await,
    )
    .await;

    assert_eq!(
        *calls.lock().unwrap(),
        vec!["put", "get", "list"],
        "every op should have reached the service exactly once, in order"
    );
}

// ---------------------------------------------------------------------------
// C1 — path traversal. `{folder}/{key}` is matched by PREFIX and never
// normalized, so a `..` segment must be refused as a malformed request before
// authorization runs at all: under an ALLOWING ctx (the shape a real grant
// produces) the op must still fail, and the service must never see it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_with_parent_traversal_key_is_invalid_argument() {
    let calls = new_calls();
    let svc = storage_fakes::RecordingStorage::new(calls.clone());
    let req = wire::storage::GetRequest {
        folder: "site/jhg".into(),
        // Composes to `site/jhg/../x` — textually beneath the folder grant.
        key: "../x".into(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::STORAGE_GET);

    let out =
        wafer_core::interfaces::storage::handler::handle_message(&svc, &AllowCtx, &msg, &body)
            .await;
    let err = expect_invalid_argument(out).await;
    assert!(
        err.message.contains("storage.get") && err.message.contains("key"),
        "the refusal must name the op and the offending component, got: {}",
        err.message
    );

    assert!(
        calls.lock().unwrap().is_empty(),
        "a traversal key must never reach the service; calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn traversal_is_refused_for_every_folder_and_key_op() {
    // Every op that takes a folder/key pair, plus the folder-only ops. Each
    // runs under the ALLOWING ctx, so an InvalidArgument here can only come
    // from the path validation.
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            ServiceOp::STORAGE_PUT,
            codec::encode(&wire::storage::PutRequest {
                folder: "site/jhg".into(),
                key: "../evil.png".into(),
                data: vec![1],
                content_type: "image/png".into(),
            })
            .unwrap(),
        ),
        (
            ServiceOp::STORAGE_GET_STREAMING,
            codec::encode(&wire::storage::GetRequest {
                folder: "site/jhg".into(),
                key: "../secret".into(),
            })
            .unwrap(),
        ),
        (
            ServiceOp::STORAGE_DELETE,
            codec::encode(&wire::storage::DeleteRequest {
                folder: "site/jhg".into(),
                key: "..".into(),
            })
            .unwrap(),
        ),
        (
            ServiceOp::STORAGE_LIST,
            codec::encode(&wire::storage::ListRequest {
                folder: "site/jhg/../other".into(),
                prefix: String::new(),
                limit: 10,
                offset: 0,
                cursor: None,
            })
            .unwrap(),
        ),
        (
            ServiceOp::STORAGE_CREATE_FOLDER,
            codec::encode(&wire::storage::CreateFolderRequest {
                name: "site/../other".into(),
                public: false,
            })
            .unwrap(),
        ),
        (
            ServiceOp::STORAGE_DELETE_FOLDER,
            codec::encode(&wire::storage::DeleteFolderRequest {
                name: "site/jhg/..".into(),
            })
            .unwrap(),
        ),
        // An empty component is the same class of malformed path.
        (
            ServiceOp::STORAGE_GET,
            codec::encode(&wire::storage::GetRequest {
                folder: "site/jhg".into(),
                key: String::new(),
            })
            .unwrap(),
        ),
    ];

    for (op, body) in cases {
        let calls = new_calls();
        let svc = storage_fakes::RecordingStorage::new(calls.clone());
        let out = wafer_core::interfaces::storage::handler::handle_message(
            &svc,
            &AllowCtx,
            &msg_without_wrap_meta(op),
            &body,
        )
        .await;
        expect_invalid_argument(out).await;
        assert!(
            calls.lock().unwrap().is_empty(),
            "{op} must not reach the service on a traversal path; calls = {:?}",
            calls.lock().unwrap()
        );
    }
}

/// The streaming upload authorizes off a header FRAME rather than a request
/// body, so it needs its own proof that the same validation runs before any
/// body frame is consumed.
#[tokio::test]
async fn put_streaming_with_traversal_key_is_invalid_argument() {
    let calls = new_calls();
    let svc = storage_fakes::RecordingStorage::new(calls.clone());
    let header = codec::encode(&wire::storage::PutStreamingHeader {
        folder: "site/jhg".into(),
        key: "../../other/secret".into(),
        content_type: "text/plain".into(),
    })
    .unwrap();
    let input = InputStream::from_stream(futures::stream::iter(vec![header, b"body".to_vec()]));

    let out = wafer_core::interfaces::storage::handler::handle_put_streaming(
        &svc,
        &AllowCtx,
        &msg_without_wrap_meta(ServiceOp::STORAGE_PUT_STREAMING),
        input,
    )
    .await;
    expect_invalid_argument(out).await;
    assert!(
        calls.lock().unwrap().is_empty(),
        "a traversal key must never reach put_streaming; calls = {:?}",
        calls.lock().unwrap()
    );
}

/// The legitimate shape still works: an ordinary nested key under an allowing
/// ctx reaches the service untouched.
#[tokio::test]
async fn nested_keys_without_traversal_still_reach_the_service() {
    let calls = new_calls();
    let svc = storage_fakes::RecordingStorage::new(calls.clone());
    let body = codec::encode(&wire::storage::GetRequest {
        folder: "site/jhg".into(),
        key: "sub/dir/a.txt".into(),
    })
    .unwrap();
    expect_success(
        wafer_core::interfaces::storage::handler::handle_message(
            &svc,
            &AllowCtx,
            &msg_without_wrap_meta(ServiceOp::STORAGE_GET),
            &body,
        )
        .await,
    )
    .await;
    assert_eq!(*calls.lock().unwrap(), vec!["get"]);
}
