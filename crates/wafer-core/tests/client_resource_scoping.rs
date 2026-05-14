//! SEC-014 / SEC-015 regression tests.
//!
//! Verify that the storage and crypto clients set `wrap.resource` to the
//! actual resource being accessed (per-file path for storage, operation name
//! for crypto), rather than passing `None`. Without these, a single
//! `Storage::*` or `Crypto::*` grant would gate every op uniformly — the
//! point of the fix is to make grants fine-grainable per path / per op.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use wafer_block::{
    context::Context,
    meta::{META_WRAP_ACCESS, META_WRAP_RESOURCE, META_WRAP_RESOURCE_TYPE},
    streams::{input::InputStream, output::OutputStream},
    Message,
};

/// Captured fields from a single `call_block` invocation.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct Captured {
    block: String,
    kind: String,
    resource: Option<String>,
    access: Option<String>,
    resource_type: Option<String>,
}

/// Context impl that records every call_block invocation and returns an
/// empty buffered response so the client can decode normally.
#[derive(Clone)]
struct RecordingContext {
    captured: Arc<Mutex<Vec<Captured>>>,
    /// Bytes to return as the response. Decoded by the client. For ops
    /// returning `()` we return an empty buffer.
    response_bytes: Arc<Vec<u8>>,
}

impl RecordingContext {
    fn new(response_bytes: Vec<u8>) -> Self {
        Self {
            captured: Arc::new(Mutex::new(Vec::new())),
            response_bytes: Arc::new(response_bytes),
        }
    }

    fn last(&self) -> Captured {
        self.captured.lock().unwrap().last().cloned().unwrap()
    }
}

#[async_trait]
impl Context for RecordingContext {
    async fn call_block(
        &self,
        block_name: &str,
        msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        let resource = {
            let r = msg.get_meta(META_WRAP_RESOURCE);
            if r.is_empty() {
                None
            } else {
                Some(r.to_string())
            }
        };
        let access = {
            let a = msg.get_meta(META_WRAP_ACCESS);
            if a.is_empty() {
                None
            } else {
                Some(a.to_string())
            }
        };
        let resource_type = {
            let rt = msg.get_meta(META_WRAP_RESOURCE_TYPE);
            if rt.is_empty() {
                None
            } else {
                Some(rt.to_string())
            }
        };
        self.captured.lock().unwrap().push(Captured {
            block: block_name.to_string(),
            kind: msg.kind,
            resource,
            access,
            resource_type,
        });
        OutputStream::respond((*self.response_bytes).clone())
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        std::sync::Arc::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Storage client — SEC-014
// ---------------------------------------------------------------------------

#[tokio::test]
async fn storage_put_passes_folder_slash_key_as_resource() {
    let ctx = RecordingContext::new(Vec::new());
    let _ =
        wafer_core::clients::storage::put(&ctx, "uploads", "img/cat.png", b"x", "image/png").await;
    let c = ctx.last();
    assert_eq!(c.resource.as_deref(), Some("uploads/img/cat.png"));
    assert_eq!(c.access.as_deref(), Some("write"));
    assert_eq!(c.resource_type.as_deref(), Some("storage"));
}

#[tokio::test]
async fn storage_delete_passes_folder_slash_key_as_resource() {
    let ctx = RecordingContext::new(Vec::new());
    let _ = wafer_core::clients::storage::delete(&ctx, "uploads", "img/cat.png").await;
    let c = ctx.last();
    assert_eq!(c.resource.as_deref(), Some("uploads/img/cat.png"));
    assert_eq!(c.access.as_deref(), Some("write"));
    assert_eq!(c.resource_type.as_deref(), Some("storage"));
}

#[tokio::test]
async fn storage_get_passes_folder_slash_key_as_resource() {
    // Encode a minimal ObjectInfo header so the buffered get() decoder
    // accepts the response. We just want to capture the meta.
    let info = wafer_block::wire::storage::ObjectInfo {
        key: "img/cat.png".to_string(),
        size: 0,
        content_type: "image/png".to_string(),
        last_modified: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
    };
    let header_bytes = wafer_block::codec::encode(&info).unwrap();
    // The client uses call_service_streaming; with a single Response frame
    // the buffered get() will try to split into header+body. For this test
    // we don't care about the decoded result, only the captured meta. We
    // accept the decode possibly erroring after capture.
    let ctx = RecordingContext::new(header_bytes);
    let _ = wafer_core::clients::storage::get(&ctx, "uploads", "img/cat.png").await;
    let c = ctx.last();
    assert_eq!(c.resource.as_deref(), Some("uploads/img/cat.png"));
    assert_eq!(c.access.as_deref(), Some("read"));
    assert_eq!(c.resource_type.as_deref(), Some("storage"));
}

#[tokio::test]
async fn storage_list_passes_folder_as_resource() {
    let list = wafer_block::wire::storage::ObjectList {
        objects: vec![],
        total_count: 0,
    };
    let resp = wafer_block::codec::encode(&list).unwrap();
    let ctx = RecordingContext::new(resp);
    let _ = wafer_core::clients::storage::list(
        &ctx,
        "uploads",
        &wafer_core::clients::storage::ListOptions::default(),
    )
    .await;
    let c = ctx.last();
    assert_eq!(c.resource.as_deref(), Some("uploads"));
    assert_eq!(c.access.as_deref(), Some("read"));
    assert_eq!(c.resource_type.as_deref(), Some("storage"));
}

// ---------------------------------------------------------------------------
// Crypto client — SEC-015
// ---------------------------------------------------------------------------

#[tokio::test]
async fn crypto_hash_passes_operation_as_resource() {
    let resp =
        wafer_block::codec::encode(&wafer_block::wire::crypto::HashResponse { hash: "x".into() })
            .unwrap();
    let ctx = RecordingContext::new(resp);
    let _ = wafer_core::clients::crypto::hash(&ctx, "pw").await;
    let c = ctx.last();
    assert_eq!(c.resource.as_deref(), Some("hash"));
    assert_eq!(c.resource_type.as_deref(), Some("crypto"));
}

#[tokio::test]
async fn crypto_compare_hash_passes_operation_as_resource() {
    let resp = wafer_block::codec::encode(&wafer_block::wire::crypto::CompareHashResponse {
        matches: true,
    })
    .unwrap();
    let ctx = RecordingContext::new(resp);
    let _ = wafer_core::clients::crypto::compare_hash(&ctx, "pw", "hash").await;
    let c = ctx.last();
    assert_eq!(c.resource.as_deref(), Some("compare_hash"));
    assert_eq!(c.resource_type.as_deref(), Some("crypto"));
}

#[tokio::test]
async fn crypto_sign_passes_operation_as_resource() {
    let resp =
        wafer_block::codec::encode(&wafer_block::wire::crypto::SignResponse { token: "t".into() })
            .unwrap();
    let ctx = RecordingContext::new(resp);
    let _ = wafer_core::clients::crypto::sign(
        &ctx,
        &HashMap::new(),
        std::time::Duration::from_secs(60),
    )
    .await;
    let c = ctx.last();
    assert_eq!(c.resource.as_deref(), Some("sign"));
    assert_eq!(c.resource_type.as_deref(), Some("crypto"));
}

#[tokio::test]
async fn crypto_verify_passes_operation_as_resource() {
    let resp = wafer_block::codec::encode(&wafer_block::wire::crypto::VerifyResponse {
        claims: HashMap::new(),
    })
    .unwrap();
    let ctx = RecordingContext::new(resp);
    let _ = wafer_core::clients::crypto::verify(&ctx, "tok").await;
    let c = ctx.last();
    assert_eq!(c.resource.as_deref(), Some("verify"));
    assert_eq!(c.resource_type.as_deref(), Some("crypto"));
}

#[tokio::test]
async fn crypto_random_bytes_passes_operation_as_resource() {
    let resp = wafer_block::codec::encode(&wafer_block::wire::crypto::RandomBytesResponse {
        bytes: vec![],
    })
    .unwrap();
    let ctx = RecordingContext::new(resp);
    let _ = wafer_core::clients::crypto::random_bytes(&ctx, 16).await;
    let c = ctx.last();
    assert_eq!(c.resource.as_deref(), Some("random_bytes"));
    assert_eq!(c.resource_type.as_deref(), Some("crypto"));
}
