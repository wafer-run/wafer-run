//! Typed client for the storage service.
//!
//! All operations except `get` use a single-frame request/response shape.
//! `get` uses a two-frame response protocol: the first frame carries the
//! encoded [`ObjectInfo`] header, subsequent frames carry the raw object
//! body bytes. Buffered helpers assemble the tuple `(Vec<u8>, ObjectInfo)`;
//! [`get_stream`] returns a [`StorageGetStream`] wrapper exposing the
//! decoded header alongside chunked body access.
//!
//! No `put_stream` helper is provided in this revision — request streaming
//! for uploads is not yet plumbed host-side.

use wafer_block::{
    codec,
    wire::storage::{
        CreateFolderRequest, DeleteFolderRequest, DeleteRequest, FolderInfo, GetRequest,
        ListRequest, ObjectInfo, ObjectList, PutRequest,
    },
    ErrorCode, Message, ServiceOp, WaferError,
};

use super::common::{collect_single_frame, consume_ack, open_buffered};
use crate::stream::{CallStream, ResponseStream};

const BLOCK: &str = "wafer-run/storage";

// ---------------------------------------------------------------------------
// Buffered ops
// ---------------------------------------------------------------------------

/// Buffered: store an object. The full body is sent as part of a single
/// request frame; the response is an empty acknowledgement frame.
pub fn put(folder: &str, key: &str, data: &[u8], content_type: &str) -> Result<(), WaferError> {
    let req = PutRequest {
        folder: folder.into(),
        key: key.into(),
        data: data.to_vec(),
        content_type: content_type.into(),
    };
    let req_bytes = codec::encode(&req)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::STORAGE_PUT, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: fetch an object, accumulating its body into a `Vec<u8>`.
///
/// Returns the body bytes alongside the [`ObjectInfo`] metadata header.
pub fn get(folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), WaferError> {
    let mut response_stream = open_get_stream(folder, key)?;
    let info = decode_get_header(&mut response_stream)?;
    let mut data = Vec::new();
    while let Some(chunk) = response_stream.next_chunk()? {
        data.extend(chunk);
    }
    Ok((data, info))
}

/// Buffered: delete an object.
pub fn delete(folder: &str, key: &str) -> Result<(), WaferError> {
    let req = DeleteRequest {
        folder: folder.into(),
        key: key.into(),
    };
    let req_bytes = codec::encode(&req)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::STORAGE_DELETE, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: list objects under a folder. Filtering / pagination fields on
/// [`ListRequest`] (`prefix`, `limit`, `offset`) are honored verbatim;
/// returns the full [`ObjectList`].
pub fn list(request: &ListRequest) -> Result<ObjectList, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::STORAGE_LIST, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "storage LIST")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding storage LIST response: {}", e.message),
        )
    })
}

/// Buffered: create a folder, optionally marking it as public.
pub fn create_folder(name: &str, public: bool) -> Result<(), WaferError> {
    let req = CreateFolderRequest {
        name: name.into(),
        public,
    };
    let req_bytes = codec::encode(&req)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::STORAGE_CREATE_FOLDER, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: delete a folder.
pub fn delete_folder(name: &str) -> Result<(), WaferError> {
    let req = DeleteFolderRequest { name: name.into() };
    let req_bytes = codec::encode(&req)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::STORAGE_DELETE_FOLDER, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: list all folders. The op takes no request body — the request
/// side is closed immediately via `finish()` (zero `write_chunk` calls).
pub fn list_folders() -> Result<Vec<FolderInfo>, WaferError> {
    let msg = Message {
        kind: ServiceOp::STORAGE_LIST_FOLDERS.to_string(),
        meta: vec![],
    };
    let call = CallStream::open(BLOCK, &msg)?;
    let mut response_stream = call.finish()?;
    let body = collect_single_frame(&mut response_stream, "storage LIST_FOLDERS")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding storage LIST_FOLDERS response: {}", e.message),
        )
    })
}

// ---------------------------------------------------------------------------
// Streaming ops
// ---------------------------------------------------------------------------

/// Streaming: fetch an object, returning a [`StorageGetStream`] that yields
/// body chunks as they arrive and exposes the [`ObjectInfo`] header.
pub fn get_stream(folder: &str, key: &str) -> Result<StorageGetStream, WaferError> {
    let mut response_stream = open_get_stream(folder, key)?;
    let info = decode_get_header(&mut response_stream)?;
    Ok(StorageGetStream {
        inner: response_stream,
        info,
    })
}

/// Streaming response wrapper for `STORAGE_GET`.
///
/// The [`ObjectInfo`] header has already been consumed from the underlying
/// stream before this wrapper is returned, so each call to
/// [`Self::next_chunk`] yields raw object body bytes.
#[must_use = "response stream must be consumed via next_chunk"]
pub struct StorageGetStream {
    inner: ResponseStream,
    info: ObjectInfo,
}

impl StorageGetStream {
    /// Object metadata header decoded from the first response frame.
    pub fn info(&self) -> &ObjectInfo {
        &self.info
    }

    /// Pull the next body chunk from the stream.
    ///
    /// Returns `Ok(Some(bytes))` for each body chunk, and `Ok(None)` once
    /// the body has been fully delivered (end-of-stream). The header was
    /// already consumed before this wrapper was constructed, so callers do
    /// not need to skip a header chunk — every chunk yielded here is body
    /// bytes.
    pub fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, WaferError> {
        self.inner.next_chunk()
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Open a STORAGE_GET stream — single-frame request, multi-frame response.
fn open_get_stream(folder: &str, key: &str) -> Result<ResponseStream, WaferError> {
    let req = GetRequest {
        folder: folder.into(),
        key: key.into(),
    };
    let req_bytes = codec::encode(&req)?;
    open_buffered(BLOCK, ServiceOp::STORAGE_GET, &req_bytes)
}

/// Pull and decode the leading [`ObjectInfo`] header frame from a
/// `STORAGE_GET` response stream. Shared by [`get`] and [`get_stream`].
fn decode_get_header(stream: &mut ResponseStream) -> Result<ObjectInfo, WaferError> {
    let header_bytes = stream.next_chunk()?.ok_or_else(|| {
        WaferError::new(
            ErrorCode::Internal,
            "stream ended before storage GET header frame",
        )
    })?;
    codec::decode(&header_bytes).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding storage GET header: {}", e.message),
        )
    })
}
