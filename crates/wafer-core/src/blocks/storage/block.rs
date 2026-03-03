use std::sync::Arc;

use serde::{Deserialize, Serialize};

use wafer_run::block::{Block, BlockInfo};
use wafer_run::common::{ErrorCode, ServiceOp};
use wafer_run::context::Context;
use super::service::{StorageError, StorageService};
use wafer_run::types::*;
use wafer_run::helpers::{respond_json, respond_empty};

/// StorageBlock wraps a StorageService and exposes it as a Block.
pub struct StorageBlock {
    service: Arc<dyn StorageService>,
}

impl StorageBlock {
    pub fn new(service: Arc<dyn StorageService>) -> Self {
        Self { service }
    }
}

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

impl Block for StorageBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer/storage".to_string(),
            version: "0.1.0".to_string(),
            interface: "wafer.infra.storage".to_string(),
            summary: "Object storage operations via StorageService".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }

    fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
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
                match self
                    .service
                    .put(&req.folder, &req.key, &req.data, &req.content_type)
                {
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
                match self.service.get(&req.folder, &req.key) {
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
                match self.service.delete(&req.folder, &req.key) {
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
                match self.service.list(&req.folder, &opts) {
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
                match self.service.create_folder(&req.name, req.public) {
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
                match self.service.delete_folder(&req.name) {
                    Ok(()) => respond_empty(msg),
                    Err(e) => Result_::error(storage_error_to_wafer(e)),
                }
            }
            ServiceOp::STORAGE_LIST_FOLDERS => {
                match self.service.list_folders() {
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

    fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}
