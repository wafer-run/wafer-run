#[cfg(not(feature = "wasm-component"))]
use std::pin::Pin;
#[cfg(not(feature = "wasm-component"))]
use std::task::{Context as TaskContext, Poll};

#[cfg(not(feature = "wasm-component"))]
use futures::Stream;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::stream::StreamEvent;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::streams::output::OutputStream;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::wire::storage::GetRequest;
// Re-export wire types for callers — byte-identical to the legacy
// `interfaces::storage::service::*` types.
pub use wafer_block::wire::storage::{FolderInfo, ObjectInfo, ObjectList};
use wafer_block::{
    common::{ErrorCode, ServiceOp},
    wire::storage::{
        CreateFolderRequest, DeleteFolderRequest, DeleteRequest, ListRequest, PutRequest,
    },
    WaferError,
};

#[cfg(not(feature = "wasm-component"))]
use super::{buffered_header_and_body, call_service_streaming, read_header_frame};
use super::{call_service, decode, dual_api, svc};
// `ListOptions` is a runtime-only ergonomic wrapper (no serde derives); keep
// it pointing at the interfaces type and convert to `ListRequest` at the wire
// boundary inside `list`.
pub use crate::interfaces::storage::service::ListOptions;

const BLOCK: &str = "wafer-run/storage";

// ===========================================================================
// Public API — generated as async (native) or sync (wasm-component)
// ===========================================================================

dual_api! {
    pub fn put(ctx, folder: &str, key: &str, data: &[u8], content_type: &str) -> Result<(), WaferError> {
        let req = PutRequest {
            folder: folder.to_string(),
            key: key.to_string(),
            data: data.to_vec(),
            content_type: content_type.to_string(),
        };
        let resource = format!("{folder}/{key}");
        svc!(ctx, BLOCK, ServiceOp::STORAGE_PUT, &req, Some(resource.as_str()), true, Some("storage"))?;
        Ok(())
    }

    pub fn delete(ctx, folder: &str, key: &str) -> Result<(), WaferError> {
        let req = DeleteRequest {
            folder: folder.to_string(),
            key: key.to_string(),
        };
        let resource = format!("{folder}/{key}");
        svc!(ctx, BLOCK, ServiceOp::STORAGE_DELETE, &req, Some(resource.as_str()), true, Some("storage"))?;
        Ok(())
    }

    pub fn list(ctx, folder: &str, opts: &ListOptions) -> Result<ObjectList, WaferError> {
        let req = ListRequest {
            folder: folder.to_string(),
            prefix: opts.prefix.clone(),
            limit: opts.limit,
            offset: opts.offset,
        };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::STORAGE_LIST,
            &req,
            Some(folder),
            false,
            Some("storage")
        )?;
        decode(&data)
    }

    pub fn create_folder(ctx, name: &str, public: bool) -> Result<(), WaferError> {
        let req = CreateFolderRequest {
            name: name.to_string(),
            public,
        };
        svc!(ctx, BLOCK, ServiceOp::STORAGE_CREATE_FOLDER, &req, Some(name), true, Some("storage"))?;
        Ok(())
    }

    pub fn delete_folder(ctx, name: &str) -> Result<(), WaferError> {
        let req = DeleteFolderRequest { name: name.to_string() };
        svc!(ctx, BLOCK, ServiceOp::STORAGE_DELETE_FOLDER, &req, Some(name), true, Some("storage"))?;
        Ok(())
    }

    pub fn list_folders(ctx,) -> Result<Vec<FolderInfo>, WaferError> {
        let data = svc!(ctx, BLOCK, ServiceOp::STORAGE_LIST_FOLDERS, &serde_json::json!({}), None, false, Some("storage"))?;
        decode(&data)
    }
}

// ---------------------------------------------------------------------------
// `get`: two-frame buffered + streaming variants (native only).
//
// The handler emits an `ObjectInfo` header chunk followed by zero-or-more body
// chunks. `get` reassembles them into the legacy `(Vec<u8>, ObjectInfo)`
// shape for ergonomic parity with the rest of the client API; `get_stream`
// returns a [`NativeStorageGetStream`] for callers that want to forward
// chunks as they arrive (large media downloads, etc.).
// ---------------------------------------------------------------------------

/// Buffered: fetch an object via `wafer-run/storage` and return the full body
/// plus its [`ObjectInfo`].
///
/// The handler emits a two-frame response: an `ObjectInfo` chunk followed by
/// zero-or-more body chunks. This helper consumes both frames and reassembles
/// them in the historical `(data, info)` order.
#[cfg(not(feature = "wasm-component"))]
pub async fn get(
    ctx: &dyn Context,
    folder: &str,
    key: &str,
) -> Result<(Vec<u8>, ObjectInfo), WaferError> {
    let req = GetRequest {
        folder: folder.to_string(),
        key: key.to_string(),
    };
    let resource = format!("{folder}/{key}");
    let out = call_service_streaming(
        ctx,
        BLOCK,
        ServiceOp::STORAGE_GET,
        &req,
        Some(resource.as_str()),
        false,
        Some("storage"),
    )
    .await?;
    let (info, body): (ObjectInfo, Vec<u8>) =
        buffered_header_and_body(out, "decoding storage GET response").await?;
    Ok((body, info))
}

/// WASM sync variant of [`get`]. Currently unimplemented — the WASM-component
/// path will be redesigned alongside the streaming protocol.
#[cfg(feature = "wasm-component")]
pub fn get(folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), WaferError> {
    let _ = (folder, key);
    Err(WaferError::new(
        ErrorCode::UNIMPLEMENTED,
        "wasm-component storage::get not yet implemented for streaming protocol",
    ))
}

/// Streaming: fetch an object and return a [`NativeStorageGetStream`] whose
/// `ObjectInfo` header is already decoded; the remaining stream yields body
/// chunks as they arrive.
///
/// Use this when the response body is large (e.g. media download) and the
/// caller wants to forward chunks immediately instead of buffering the full
/// body in memory.
#[cfg(not(feature = "wasm-component"))]
pub async fn get_stream(
    ctx: &dyn Context,
    folder: &str,
    key: &str,
) -> Result<NativeStorageGetStream, WaferError> {
    let req = GetRequest {
        folder: folder.to_string(),
        key: key.to_string(),
    };
    let resource = format!("{folder}/{key}");
    let mut out = call_service_streaming(
        ctx,
        BLOCK,
        ServiceOp::STORAGE_GET,
        &req,
        Some(resource.as_str()),
        false,
        Some("storage"),
    )
    .await?;
    let info: ObjectInfo = read_header_frame(&mut out, "decoding storage GET response").await?;
    Ok(NativeStorageGetStream {
        inner: out,
        info,
        finished: false,
    })
}

/// Streaming response wrapper for [`get_stream`].
///
/// The underlying `OutputStream` has already had its `ObjectInfo` header
/// frame consumed before this wrapper is returned, so calls to
/// [`futures::Stream::poll_next`] yield body bytes directly. Non-`Complete`
/// terminal events are translated to a single `Err` item, after which the
/// stream returns `None`.
#[cfg(not(feature = "wasm-component"))]
#[must_use = "storage GET stream must be consumed"]
pub struct NativeStorageGetStream {
    inner: OutputStream,
    info: ObjectInfo,
    finished: bool,
}

#[cfg(not(feature = "wasm-component"))]
impl NativeStorageGetStream {
    /// Object metadata from the response header.
    pub fn info(&self) -> &ObjectInfo {
        &self.info
    }
}

#[cfg(not(feature = "wasm-component"))]
impl Stream for NativeStorageGetStream {
    type Item = Result<Vec<u8>, WaferError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        if self.finished {
            return Poll::Ready(None);
        }
        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(WaferError::new(
                        ErrorCode::INTERNAL,
                        "storage GET stream ended without terminal event",
                    ))));
                }
                Poll::Ready(Some(StreamEvent::Chunk(bytes))) => {
                    return Poll::Ready(Some(Ok(bytes)));
                }
                Poll::Ready(Some(StreamEvent::Meta(_))) => continue,
                Poll::Ready(Some(StreamEvent::Complete { .. })) => {
                    self.finished = true;
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(StreamEvent::Error(e))) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(*e)));
                }
                Poll::Ready(Some(StreamEvent::Drop)) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(WaferError::new(
                        ErrorCode::INTERNAL,
                        "storage handler returned Drop",
                    ))));
                }
                Poll::Ready(Some(StreamEvent::Continue(msg))) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(WaferError::new(
                        ErrorCode::INTERNAL,
                        format!("storage handler returned Continue (kind: {})", msg.kind),
                    ))));
                }
            }
        }
    }
}
