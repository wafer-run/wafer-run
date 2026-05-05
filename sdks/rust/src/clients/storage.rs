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
    let mut response_stream = open_buffered(ServiceOp::STORAGE_PUT, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: fetch an object, accumulating its body into a `Vec<u8>`.
///
/// Returns the body bytes alongside the [`ObjectInfo`] metadata header.
pub fn get(folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), WaferError> {
    let mut response_stream = open_get_stream(folder, key)?;
    let header_bytes = response_stream
        .next_chunk()?
        .ok_or_else(|| WaferError::new(ErrorCode::Internal, "stream ended before header frame"))?;
    let info: ObjectInfo = codec::decode(&header_bytes).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding storage GET header: {}", e.message),
        )
    })?;
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
    let mut response_stream = open_buffered(ServiceOp::STORAGE_DELETE, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: list objects under a folder, optionally filtered by `prefix`
/// and paginated by `limit` / `offset`. Returns the full [`ObjectList`].
pub fn list(folder: &str, prefix: &str, limit: i64, offset: i64) -> Result<ObjectList, WaferError> {
    let req = ListRequest {
        folder: folder.into(),
        prefix: prefix.into(),
        limit,
        offset,
    };
    let req_bytes = codec::encode(&req)?;
    let mut response_stream = open_buffered(ServiceOp::STORAGE_LIST, &req_bytes)?;
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
    let mut response_stream = open_buffered(ServiceOp::STORAGE_CREATE_FOLDER, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: delete a folder.
pub fn delete_folder(name: &str) -> Result<(), WaferError> {
    let req = DeleteFolderRequest { name: name.into() };
    let req_bytes = codec::encode(&req)?;
    let mut response_stream = open_buffered(ServiceOp::STORAGE_DELETE_FOLDER, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: list all folders. The op takes no arguments — an empty
/// request frame is sent before `finish`.
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
    let header_bytes = response_stream
        .next_chunk()?
        .ok_or_else(|| WaferError::new(ErrorCode::Internal, "stream ended before header frame"))?;
    let info: ObjectInfo = codec::decode(&header_bytes).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding storage GET header: {}", e.message),
        )
    })?;
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

/// Send a single-frame request and return the response stream.
fn open_buffered(op: &str, req_bytes: &[u8]) -> Result<ResponseStream, WaferError> {
    let msg = Message {
        kind: op.to_string(),
        meta: vec![],
    };
    let mut call = CallStream::open(BLOCK, &msg)?;
    call.write_chunk(req_bytes)?;
    call.finish()
}

/// Open a STORAGE_GET stream — single-frame request, multi-frame response.
fn open_get_stream(folder: &str, key: &str) -> Result<ResponseStream, WaferError> {
    let req = GetRequest {
        folder: folder.into(),
        key: key.into(),
    };
    let req_bytes = codec::encode(&req)?;
    open_buffered(ServiceOp::STORAGE_GET, &req_bytes)
}

/// Drain a single-frame ack response. The frame's payload is ignored
/// (handlers may choose to send an empty body); end-of-stream is then
/// asserted before returning.
fn consume_ack(response_stream: &mut ResponseStream) -> Result<(), WaferError> {
    // Pull and discard the ack frame, if any.
    let _ = response_stream.next_chunk()?;
    // Drain any further frames (handlers should not emit them, but be
    // tolerant here — the stream's Drop will close the handle either way).
    while response_stream.next_chunk()?.is_some() {}
    Ok(())
}

/// Collect a single response frame whose payload carries an encoded value.
/// Errors if the stream ended without a frame.
fn collect_single_frame(
    response_stream: &mut ResponseStream,
    context: &str,
) -> Result<Vec<u8>, WaferError> {
    let body = response_stream.next_chunk()?.ok_or_else(|| {
        WaferError::new(
            ErrorCode::Internal,
            format!("{context}: stream ended before response frame"),
        )
    })?;
    // Drain any trailing frames defensively.
    while response_stream.next_chunk()?.is_some() {}
    Ok(body)
}
