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
use crate::interfaces::handler_util::{
    decode_and_authorize_checked, stream_with_header, to_output,
};

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

// --- Path resolution ---------------------------------------------------------

/// Resolve the `folder` (or folder `name`) a caller sent into the one path
/// the backend touches — the same string WRAP authorizes.
///
/// A folder is addressed in one of two ways:
/// - **Caller-relative** (no leading `@`): the folder lives in the calling
///   block's own namespace. `uploads` resolves to `{caller}/uploads`, and the
///   empty folder resolves to `{caller}` itself (the namespace root). The
///   caller is [`Context::caller_id`], the registration name of the block that
///   made the call, so a block cannot name its way into another block's
///   namespace: `other-org/other-block/secrets` from `acme/app` resolves to
///   `acme/app/other-org/other-block/secrets`.
/// - **Explicit** (`@{org}/{block}/…`): the path after the `@`, as written.
///   WRAP admits it when its `{org}/{block}` owner is the caller, when the
///   caller is the admin block, or when a Storage grant covers it.
///
/// The resolved path is then refused if any `/`-separated segment is empty,
/// `.` or `..` ([`wafer_block::wrap::is_traversal_safe_path`]). Storage
/// authorization is textual and prefix-based and nothing normalizes the
/// string, so `uploads/../../other-org/x` would sit under the caller's own
/// namespace as text while naming another block's folder. Such a request is
/// malformed and is refused rather than normalized: rewriting it would store
/// or return an object the caller did not ask for.
///
/// A caller-relative folder with no caller to scope it to (a top-level call)
/// is `PermissionDenied`: there is no namespace it could belong to.
///
/// `op` and `what` label the error (`"storage.get"`, `"folder"`). Public so a
/// block wrapping the storage block (an access log, say) reports the same
/// path the handler touches instead of re-deriving it.
pub fn resolve_folder(
    ctx: &dyn Context,
    op: &str,
    what: &str,
    folder: &str,
) -> Result<String, WaferError> {
    let resolved = match folder.strip_prefix('@') {
        Some(explicit) => explicit.to_string(),
        None => {
            let Some(caller) = ctx.caller_id() else {
                return Err(WaferError::new(
                    ErrorCode::PermissionDenied,
                    format!(
                        "{op}: `{what}` {folder:?} is relative to the calling block's \
                         namespace, and this call has no calling block"
                    ),
                ));
            };
            if folder.is_empty() {
                caller.to_string()
            } else {
                format!("{caller}/{folder}")
            }
        }
    };
    check_path(op, what, folder, &resolved)?;
    Ok(resolved)
}

/// Refuse `path` unless every `/`-separated segment is a plain name, naming
/// `sent` — the value as the caller wrote it — in the `InvalidArgument`.
fn check_path(op: &str, what: &str, sent: &str, path: &str) -> Result<(), WaferError> {
    if wafer_block::wrap::is_traversal_safe_path(path) {
        return Ok(());
    }
    Err(WaferError::new(
        ErrorCode::InvalidArgument,
        format!(
            "invalid {op} request: `{what}` must be a plain `/`-separated path \
             with no empty, `.` or `..` segment (got {sent:?})"
        ),
    ))
}

/// An object's resolved folder and the `"{folder}/{key}"` resource an object
/// op authorizes on and the backend stores under.
struct ObjectPath {
    folder: String,
    resource: String,
}

/// Resolve an object op's `folder` ([`resolve_folder`]) and validate its
/// `key`, which is always relative to that folder.
fn resolve_object(
    ctx: &dyn Context,
    op: &str,
    folder: &str,
    key: &str,
) -> Result<ObjectPath, WaferError> {
    let folder = resolve_folder(ctx, op, "folder", folder)?;
    check_path(op, "key", key, key)?;
    let resource = format!("{folder}/{key}");
    Ok(ObjectPath { folder, resource })
}

/// Handle a storage message using the given service.
///
/// `ctx` is the trusted host-side authorization surface: every op arm that
/// touches a WRAP-governed resource authorizes via
/// [`decode_and_authorize_checked`], which bundles the codec decode with a
/// call to `ctx.check_resource_access` so an arm cannot obtain its typed
/// request without also being checked. Each arm first resolves the
/// caller-supplied folder into a backend path ([`resolve_folder`]): a plain
/// folder is scoped into the calling block's own namespace, an `@`-prefixed
/// one names a namespace explicitly, and a traversal shape is
/// `InvalidArgument` before authorization. The arm then authorizes that
/// resolved path and hands the same path to the service, so the path the
/// backend touches is always the path WRAP admitted.
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
            let (req, path) = match decode_and_authorize_checked::<wire::PutRequest, _>(
                ctx,
                body,
                "storage.put",
                |r| {
                    let path = resolve_object(ctx, "storage.put", &r.folder, &r.key)?;
                    let resource = path.resource.clone();
                    Ok((path, resource, ResourceType::Storage, ResourceAccess::Write))
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service
                .put(&path.folder, &req.key, &req.data, &req.content_type)
                .await
            {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_GET => {
            let (req, path) = match decode_and_authorize_checked::<wire::GetRequest, _>(
                ctx,
                body,
                "storage.get",
                |r| {
                    let path = resolve_object(ctx, "storage.get", &r.folder, &r.key)?;
                    let resource = path.resource.clone();
                    Ok((path, resource, ResourceType::Storage, ResourceAccess::Read))
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.get(&path.folder, &req.key).await {
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
            let (req, path) = match decode_and_authorize_checked::<wire::GetRequest, _>(
                ctx,
                body,
                "storage.get_streaming",
                |r| {
                    let path = resolve_object(ctx, "storage.get_streaming", &r.folder, &r.key)?;
                    let resource = path.resource.clone();
                    Ok((path, resource, ResourceType::Storage, ResourceAccess::Read))
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.get_streaming(&path.folder, &req.key).await {
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
            let (req, path) = match decode_and_authorize_checked::<wire::DeleteRequest, _>(
                ctx,
                body,
                "storage.delete",
                |r| {
                    let path = resolve_object(ctx, "storage.delete", &r.folder, &r.key)?;
                    let resource = path.resource.clone();
                    Ok((path, resource, ResourceType::Storage, ResourceAccess::Write))
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.delete(&path.folder, &req.key).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_LIST => {
            let (req, folder) = match decode_and_authorize_checked::<wire::ListRequest, _>(
                ctx,
                body,
                "storage.list",
                |r| {
                    let folder = resolve_folder(ctx, "storage.list", "folder", &r.folder)?;
                    let resource = folder.clone();
                    Ok((
                        folder,
                        resource,
                        ResourceType::Storage,
                        ResourceAccess::Read,
                    ))
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let opts = super::service::ListOptions {
                prefix: req.prefix,
                limit: req.limit,
                offset: req.offset,
                cursor: req.cursor,
            };
            match service.list(&folder, &opts).await {
                Ok(list) => to_output(service_object_list_to_wire(list)),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_CREATE_FOLDER => {
            let (req, name) = match decode_and_authorize_checked::<wire::CreateFolderRequest, _>(
                ctx,
                body,
                "storage.create_folder",
                |r| {
                    let name = resolve_folder(ctx, "storage.create_folder", "name", &r.name)?;
                    let resource = name.clone();
                    Ok((name, resource, ResourceType::Storage, ResourceAccess::Write))
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.create_folder(&name, req.public).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_DELETE_FOLDER => {
            let (_, name) = match decode_and_authorize_checked::<wire::DeleteFolderRequest, _>(
                ctx,
                body,
                "storage.delete_folder",
                |r| {
                    let name = resolve_folder(ctx, "storage.delete_folder", "name", &r.name)?;
                    let resource = name.clone();
                    Ok((name, resource, ResourceType::Storage, ResourceAccess::Write))
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.delete_folder(&name).await {
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
                ResourceAccess::Read,
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
/// the buffered [`ServiceOp::STORAGE_PUT`] — a WRITE of the resolved
/// `{folder}/{key}` ([`resolve_folder`]) — decoded from the header frame and
/// checked BEFORE any body frame is consumed or written. So the streaming upload can never be reached with a weaker (or
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
    let (header, path) = match decode_and_authorize_checked::<wire::PutStreamingHeader, _>(
        ctx,
        &header_bytes,
        "storage.put_streaming",
        |h| {
            let path = resolve_object(ctx, "storage.put_streaming", &h.folder, &h.key)?;
            let resource = path.resource.clone();
            Ok((path, resource, ResourceType::Storage, ResourceAccess::Write))
        },
    ) {
        Ok(h) => h,
        Err(out) => return out,
    };

    // The remaining frames are the object body. `input` is now positioned at
    // the first body chunk (its cancellation token is preserved), so
    // `put_streaming` receives a live body stream — never a buffered blob.
    match service
        .put_streaming(&path.folder, &header.key, input, &header.content_type)
        .await
    {
        Ok(()) => OutputStream::respond(vec![]),
        Err(e) => OutputStream::error(storage_error_to_wafer(e)),
    }
}
