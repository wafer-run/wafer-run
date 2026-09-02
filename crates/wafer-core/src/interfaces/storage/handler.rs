//! Shared message handler logic for storage blocks.
//!
//! Any block implementing the `storage@v1` interface can delegate to these
//! functions to avoid duplicating the message protocol handling.

use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    types::ResourceType,
    wire::storage as wire,
    *,
};

use super::service::{StorageError, StorageService};
use crate::interfaces::handler_util::{decode_and_authorize, stream_with_header, to_output};

// --- Helpers ---

fn storage_error_to_wafer(e: StorageError) -> WaferError {
    match e {
        StorageError::NotFound => WaferError::new(ErrorCode::NotFound, "object not found"),
        StorageError::Internal(msg) => WaferError::new(ErrorCode::Internal, msg),
        StorageError::Other(err) => WaferError::new(ErrorCode::Internal, err.to_string()),
    }
}

/// Convert the runtime `ObjectInfo` (declared on the service trait) into the
/// wire form. Field-identical; explicit conversion keeps the wire boundary
/// from leaking into the service trait.
fn service_object_info_to_wire(info: super::service::ObjectInfo) -> wire::ObjectInfo {
    wire::ObjectInfo {
        key: info.key,
        size: info.size,
        content_type: info.content_type,
        last_modified: info.last_modified,
    }
}

fn service_object_list_to_wire(list: super::service::ObjectList) -> wire::ObjectList {
    wire::ObjectList {
        objects: list
            .objects
            .into_iter()
            .map(service_object_info_to_wire)
            .collect(),
        total_count: list.total_count,
        next_cursor: list.next_cursor,
    }
}

fn service_folder_info_to_wire(info: super::service::FolderInfo) -> wire::FolderInfo {
    wire::FolderInfo {
        name: info.name,
        public: info.public,
        created_at: info.created_at,
    }
}

/// Handle a storage message using the given service.
///
/// `ctx` is the trusted host-side authorization surface: every op arm that
/// touches a WRAP-governed resource authorizes via
/// [`decode_and_authorize`], which bundles the codec decode with a call to
/// `ctx.check_resource_access` so an arm cannot obtain its typed request
/// without also being checked.
///
/// Wire protocol:
/// - `STORAGE_GET` emits **two frames**: a [`wire::ObjectInfo`] header chunk
///   followed by the body bytes. The body chunk is omitted when empty
///   (zero chunks → empty body on the consumer side).
/// - `STORAGE_GET_STREAMING` emits the SAME two-frame shape — a
///   [`wire::ObjectInfo`] header chunk followed by zero-or-more body chunks —
///   but the body chunks are forwarded verbatim from the service's
///   `get_streaming` stream as they arrive, so a large object is never
///   buffered whole. It authorizes identically to `STORAGE_GET`.
/// - All other ops emit a single frame: either an empty ack (PUT, DELETE,
///   CREATE_FOLDER, DELETE_FOLDER) or an encoded response (LIST, LIST_FOLDERS).
///
/// Both GET ops emit a [`wafer_block::stream::raw_frames_marker`] `Meta` event
/// between the header and the body: the object body is opaque application
/// bytes, not a codec-encoded DTO, so a consumer that re-encodes frames for a
/// guest on a different host codec must forward it verbatim. Consumers that
/// concatenate body chunks (the native clients) skip `Meta` events already and
/// are unaffected.
pub async fn handle_message(
    service: &dyn StorageService,
    ctx: &dyn Context,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::STORAGE_PUT => {
            let req =
                match decode_and_authorize::<wire::PutRequest>(ctx, body, "storage.put", |r| {
                    (
                        format!("{}/{}", r.folder, r.key),
                        ResourceType::Storage,
                        true,
                    )
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            match service
                .put(&req.folder, &req.key, &req.data, &req.content_type)
                .await
            {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_GET => {
            let req =
                match decode_and_authorize::<wire::GetRequest>(ctx, body, "storage.get", |r| {
                    (
                        format!("{}/{}", r.folder, r.key),
                        ResourceType::Storage,
                        false,
                    )
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            match service.get(&req.folder, &req.key).await {
                Ok((data, info)) => {
                    let header = service_object_info_to_wire(info);
                    OutputStream::from_producer(|sink, _cancel| async move {
                        let header_bytes = match codec::encode(&header) {
                            Ok(b) => b,
                            Err(e) => {
                                let _ = sink
                                    .error(WaferError::new(
                                        ErrorCode::Internal,
                                        format!("encoding storage GET header: {}", e.message),
                                    ))
                                    .await;
                                return;
                            }
                        };
                        if sink.send_chunk(header_bytes).await.is_err() {
                            return;
                        }
                        // Everything after this marker is the object body:
                        // raw bytes, not a wire DTO.
                        if sink.send_meta(stream::raw_frames_marker()).await.is_err() {
                            return;
                        }
                        // Body is the second frame. Skip the chunk entirely
                        // when empty — consumers reconstruct an empty body
                        // from zero chunks.
                        if !data.is_empty() && sink.send_chunk(data).await.is_err() {
                            return;
                        }
                        let _ = sink.complete(vec![]).await;
                    })
                }
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_GET_STREAMING => {
            // Same request shape and WRAP authorization as `STORAGE_GET` — a
            // read of `{folder}/{key}` — so the streaming download can never be
            // reached with a weaker grant than the buffered download.
            let req = match decode_and_authorize::<wire::GetRequest>(
                ctx,
                body,
                "storage.get_streaming",
                |r| {
                    (
                        format!("{}/{}", r.folder, r.key),
                        ResourceType::Storage,
                        false,
                    )
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.get_streaming(&req.folder, &req.key).await {
                Ok((body_stream, info)) => {
                    // Two-frame response: an `ObjectInfo` header chunk followed
                    // by the body forwarded verbatim from the service's stream
                    // (never collapsed via `collect_buffered`).
                    let header = service_object_info_to_wire(info);
                    stream_with_header(header, body_stream, "storage.get_streaming")
                }
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_DELETE => {
            let req = match decode_and_authorize::<wire::DeleteRequest>(
                ctx,
                body,
                "storage.delete",
                |r| {
                    (
                        format!("{}/{}", r.folder, r.key),
                        ResourceType::Storage,
                        true,
                    )
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.delete(&req.folder, &req.key).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_LIST => {
            let req =
                match decode_and_authorize::<wire::ListRequest>(ctx, body, "storage.list", |r| {
                    (r.folder.clone(), ResourceType::Storage, false)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            let opts = super::service::ListOptions {
                prefix: req.prefix,
                limit: req.limit,
                offset: req.offset,
                cursor: req.cursor,
            };
            match service.list(&req.folder, &opts).await {
                Ok(list) => to_output(service_object_list_to_wire(list)),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_CREATE_FOLDER => {
            let req = match decode_and_authorize::<wire::CreateFolderRequest>(
                ctx,
                body,
                "storage.create_folder",
                |r| (r.name.clone(), ResourceType::Storage, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.create_folder(&req.name, req.public).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_DELETE_FOLDER => {
            let req = match decode_and_authorize::<wire::DeleteFolderRequest>(
                ctx,
                body,
                "storage.delete_folder",
                |r| (r.name.clone(), ResourceType::Storage, true),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.delete_folder(&req.name).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_LIST_FOLDERS => {
            // Global folder enumeration is admin-only (no per-folder scope
            // exists to authorize against) — gated by the list-all sentinel.
            if let Err(e) = ctx.check_resource_access(
                wafer_block::wrap::STORAGE_LIST_ALL_RESOURCE,
                ResourceType::Storage,
                false,
            ) {
                return OutputStream::error(e);
            }
            match service.list_folders().await {
                Ok(folders) => {
                    let wire_folders: Vec<wire::FolderInfo> = folders
                        .into_iter()
                        .map(service_folder_info_to_wire)
                        .collect();
                    to_output(&wire_folders)
                }
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("unknown storage operation: {other}"),
        )),
    }
}

/// Streaming-ingress handler for [`ServiceOp::STORAGE_PUT_STREAMING`].
///
/// Unlike the buffered ops routed through [`handle_message`], the streaming
/// PUT request arrives as a stream of frames: a [`wire::PutStreamingHeader`]
/// header frame (folder / key / content_type) followed by zero-or-more raw
/// body-chunk frames. The `service_block!` ingress macro routes this op here
/// with the request `InputStream` intact — WITHOUT `collect_to_bytes` — so a
/// large object streams into the backend via [`StorageService::put_streaming`]
/// instead of being buffered whole in isolate memory.
///
/// WRAP authorization parity (security-critical): the caller is authorized for
/// the IDENTICAL `(resource, ResourceType::Storage, is_write = true)` tuple as
/// the buffered [`ServiceOp::STORAGE_PUT`] — a WRITE of `{folder}/{key}` —
/// decoded from the header frame and checked BEFORE any body frame is consumed
/// or written. So the streaming upload can never be reached with a weaker (or
/// read-only) grant than the buffered upload.
pub async fn handle_put_streaming(
    service: &dyn StorageService,
    ctx: &dyn Context,
    _msg: &Message,
    input: InputStream,
) -> OutputStream {
    use futures::StreamExt;

    let mut input = input;
    // Frame 1 is the header. An empty stream (no header frame at all) is a
    // malformed request — reject before touching the service.
    let Some(header_bytes) = input.next().await else {
        return OutputStream::error(WaferError::new(
            ErrorCode::InvalidArgument,
            "storage.put_streaming: request stream ended before the header frame",
        ));
    };

    // Decode + authorize the header BEFORE consuming any body frame. Same
    // resource tuple as the buffered `storage.put` write, so the check can't
    // be forgotten and can't be weaker than the buffered path.
    let header = match decode_and_authorize::<wire::PutStreamingHeader>(
        ctx,
        &header_bytes,
        "storage.put_streaming",
        |h| {
            (
                format!("{}/{}", h.folder, h.key),
                ResourceType::Storage,
                true,
            )
        },
    ) {
        Ok(h) => h,
        Err(out) => return out,
    };

    // The remaining frames are the object body. `input` is now positioned at
    // the first body chunk (its cancellation token is preserved), so
    // `put_streaming` receives a live body stream — never a buffered blob.
    match service
        .put_streaming(&header.folder, &header.key, input, &header.content_type)
        .await
    {
        Ok(()) => OutputStream::respond(vec![]),
        Err(e) => OutputStream::error(storage_error_to_wafer(e)),
    }
}
