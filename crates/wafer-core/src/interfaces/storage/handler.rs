//! Shared message handler logic for storage blocks.
//!
//! Any block implementing the `storage@v1` interface can delegate to these
//! functions to avoid duplicating the message protocol handling.

use serde::{Deserialize, Serialize};

use wafer_block::common::{ErrorCode, ServiceOp};
use wafer_block::streams::output::OutputStream;
use wafer_block::*;

use super::service::{StorageError, StorageService};

// --- Request types ---

#[derive(Deserialize)]
struct PutRequest {
    folder: String,
    key: String,
    data: Vec<u8>,
    #[serde(default = "default_content_type")]
    content_type: String,
}

fn default_content_type() -> String {
    "application/octet-stream".to_string()
}

#[derive(Deserialize)]
struct GetRequest {
    folder: String,
    key: String,
}

#[derive(Deserialize)]
struct DeleteRequest {
    folder: String,
    key: String,
}

#[derive(Deserialize)]
struct ListRequest {
    folder: String,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

#[derive(Deserialize)]
struct CreateFolderRequest {
    name: String,
    #[serde(default)]
    public: bool,
}

#[derive(Deserialize)]
struct DeleteFolderRequest {
    name: String,
}

// --- Response types ---

#[derive(Serialize)]
struct GetResponse {
    data: Vec<u8>,
    info: super::service::ObjectInfo,
}

// --- Helpers ---

fn storage_error_to_wafer(e: StorageError) -> WaferError {
    match e {
        StorageError::NotFound => WaferError::new(ErrorCode::NOT_FOUND, "object not found"),
        StorageError::Internal(msg) => WaferError::new(ErrorCode::INTERNAL, msg),
        StorageError::Other(err) => WaferError::new(ErrorCode::INTERNAL, err.to_string()),
    }
}

use crate::interfaces::handler_util::{decode_or_err, to_output};

/// Handle a storage message using the given service.
pub async fn handle_message(
    service: &dyn StorageService,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::STORAGE_PUT => {
            let req = decode_or_err!(body, PutRequest, "storage.put");
            match service
                .put(&req.folder, &req.key, &req.data, &req.content_type)
                .await
            {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_GET => {
            let req = decode_or_err!(body, GetRequest, "storage.get");
            match service.get(&req.folder, &req.key).await {
                Ok((data, info)) => to_output(&GetResponse { data, info }),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_DELETE => {
            let req = decode_or_err!(body, DeleteRequest, "storage.delete");
            match service.delete(&req.folder, &req.key).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_LIST => {
            let req = decode_or_err!(body, ListRequest, "storage.list");
            let opts = super::service::ListOptions {
                prefix: req.prefix,
                limit: req.limit,
                offset: req.offset,
            };
            match service.list(&req.folder, &opts).await {
                Ok(list) => to_output(&list),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_CREATE_FOLDER => {
            let req = decode_or_err!(body, CreateFolderRequest, "storage.create_folder");
            match service.create_folder(&req.name, req.public).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_DELETE_FOLDER => {
            let req = decode_or_err!(body, DeleteFolderRequest, "storage.delete_folder");
            match service.delete_folder(&req.name).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_LIST_FOLDERS => match service.list_folders().await {
            Ok(folders) => to_output(&folders),
            Err(e) => OutputStream::error(storage_error_to_wafer(e)),
        },
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown storage operation: {other}"),
        )),
    }
}
