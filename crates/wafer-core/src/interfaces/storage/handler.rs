//! Shared message handler logic for storage blocks.
//!
//! Any block implementing the `storage@v1` interface can delegate to these
//! functions to avoid duplicating the message protocol handling.

use serde::{Deserialize, Serialize};

use wafer_run::common::{ErrorCode, ServiceOp};
use wafer_run::helpers::{respond_empty, respond_json};
use wafer_run::types::*;

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

/// Handle a storage message using the given service.
pub async fn handle_message(service: &dyn StorageService, msg: &mut Message) -> Result_ {
    match msg.kind.as_str() {
        ServiceOp::STORAGE_PUT => {
            let req: PutRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid storage.put request: {e}"),
                    ))
                }
            };
            match service.put(&req.folder, &req.key, &req.data, &req.content_type).await {
                Ok(()) => respond_empty(msg),
                Err(e) => Result_::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_GET => {
            let req: GetRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid storage.get request: {e}"),
                    ))
                }
            };
            match service.get(&req.folder, &req.key).await {
                Ok((data, info)) => respond_json(msg, &GetResponse { data, info }),
                Err(e) => Result_::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_DELETE => {
            let req: DeleteRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid storage.delete request: {e}"),
                    ))
                }
            };
            match service.delete(&req.folder, &req.key).await {
                Ok(()) => respond_empty(msg),
                Err(e) => Result_::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_LIST => {
            let req: ListRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid storage.list request: {e}"),
                    ))
                }
            };
            let opts = super::service::ListOptions {
                prefix: req.prefix,
                limit: req.limit,
                offset: req.offset,
            };
            match service.list(&req.folder, &opts).await {
                Ok(list) => respond_json(msg, &list),
                Err(e) => Result_::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_CREATE_FOLDER => {
            let req: CreateFolderRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid storage.create_folder request: {e}"),
                    ))
                }
            };
            match service.create_folder(&req.name, req.public).await {
                Ok(()) => respond_empty(msg),
                Err(e) => Result_::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_DELETE_FOLDER => {
            let req: DeleteFolderRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid storage.delete_folder request: {e}"),
                    ))
                }
            };
            match service.delete_folder(&req.name).await {
                Ok(()) => respond_empty(msg),
                Err(e) => Result_::error(storage_error_to_wafer(e)),
            }
        }
        ServiceOp::STORAGE_LIST_FOLDERS => {
            match service.list_folders().await {
                Ok(folders) => respond_json(msg, &folders),
                Err(e) => Result_::error(storage_error_to_wafer(e)),
            }
        }
        other => Result_::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown storage operation: {other}"),
        )),
    }
}
