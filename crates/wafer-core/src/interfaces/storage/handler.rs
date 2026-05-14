//! Shared message handler logic for storage blocks.
//!
//! Any block implementing the `storage@v1` interface can delegate to these
//! functions to avoid duplicating the message protocol handling.

use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    meta::META_WRAP_RESOURCE,
    streams::output::OutputStream,
    wire::storage as wire,
    *,
};

use super::service::{StorageError, StorageService};
use crate::interfaces::handler_util::{decode_or_err, to_output};

/// SEC-003: enforce that the caller-supplied `wrap.resource` meta matches the
/// resource named in the decoded payload. If the meta is absent the runtime
/// already skipped WRAP entirely (legacy path) — accept; the client wrappers
/// always set this meta post-SEC-014.
fn check_resource(msg: &Message, expected: &str) -> Result<(), WaferError> {
    let supplied = msg.get_meta(META_WRAP_RESOURCE);
    if supplied.is_empty() || supplied == expected {
        Ok(())
    } else {
        Err(WaferError::new(
            ErrorCode::PERMISSION_DENIED,
            format!(
                "WRAP: wrap.resource meta '{supplied}' does not match payload resource '{expected}'"
            ),
        ))
    }
}

// --- Helpers ---

fn storage_error_to_wafer(e: StorageError) -> WaferError {
    match e {
        StorageError::NotFound => WaferError::new(ErrorCode::NOT_FOUND, "object not found"),
        StorageError::Internal(msg) => WaferError::new(ErrorCode::INTERNAL, msg),
        StorageError::Other(err) => WaferError::new(ErrorCode::INTERNAL, err.to_string()),
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
/// Wire protocol:
/// - `STORAGE_GET` emits **two frames**: a [`wire::ObjectInfo`] header chunk
///   followed by the body bytes. The body chunk is omitted when empty
///   (zero chunks → empty body on the consumer side).
/// - All other ops emit a single frame: either an empty ack (PUT, DELETE,
///   CREATE_FOLDER, DELETE_FOLDER) or an encoded response (LIST, LIST_FOLDERS).
pub async fn handle_message(
    service: &dyn StorageService,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::STORAGE_PUT => {
            let req = decode_or_err!(body, wire::PutRequest, "storage.put");
            if let Err(e) = check_resource(msg, &format!("{}/{}", req.folder, req.key)) {
                return OutputStream::error(e);
            }
            match service
                .put(&req.folder, &req.key, &req.data, &req.content_type)
                .await
            {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_GET => {
            let req = decode_or_err!(body, wire::GetRequest, "storage.get");
            if let Err(e) = check_resource(msg, &format!("{}/{}", req.folder, req.key)) {
                return OutputStream::error(e);
            }
            match service.get(&req.folder, &req.key).await {
                Ok((data, info)) => {
                    let header = service_object_info_to_wire(info);
                    OutputStream::from_producer(|sink, _cancel| async move {
                        let header_bytes = match codec::encode(&header) {
                            Ok(b) => b,
                            Err(e) => {
                                let _ = sink
                                    .error(WaferError::new(
                                        ErrorCode::INTERNAL,
                                        format!("encoding storage GET header: {}", e.message),
                                    ))
                                    .await;
                                return;
                            }
                        };
                        if sink.send_chunk(header_bytes).await.is_err() {
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
        ServiceOp::STORAGE_DELETE => {
            let req = decode_or_err!(body, wire::DeleteRequest, "storage.delete");
            if let Err(e) = check_resource(msg, &format!("{}/{}", req.folder, req.key)) {
                return OutputStream::error(e);
            }
            match service.delete(&req.folder, &req.key).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_LIST => {
            let req = decode_or_err!(body, wire::ListRequest, "storage.list");
            if let Err(e) = check_resource(msg, &req.folder) {
                return OutputStream::error(e);
            }
            let opts = super::service::ListOptions {
                prefix: req.prefix,
                limit: req.limit,
                offset: req.offset,
            };
            match service.list(&req.folder, &opts).await {
                Ok(list) => to_output(service_object_list_to_wire(list)),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_CREATE_FOLDER => {
            let req = decode_or_err!(body, wire::CreateFolderRequest, "storage.create_folder");
            if let Err(e) = check_resource(msg, &req.name) {
                return OutputStream::error(e);
            }
            match service.create_folder(&req.name, req.public).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_DELETE_FOLDER => {
            let req = decode_or_err!(body, wire::DeleteFolderRequest, "storage.delete_folder");
            if let Err(e) = check_resource(msg, &req.name) {
                return OutputStream::error(e);
            }
            match service.delete_folder(&req.name).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_LIST_FOLDERS => match service.list_folders().await {
            Ok(folders) => {
                let wire_folders: Vec<wire::FolderInfo> = folders
                    .into_iter()
                    .map(service_folder_info_to_wire)
                    .collect();
                to_output(&wire_folders)
            }
            Err(e) => OutputStream::error(storage_error_to_wafer(e)),
        },
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown storage operation: {other}"),
        )),
    }
}
