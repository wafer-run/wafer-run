use serde::{Deserialize, Serialize};

use wafer_run::common::ServiceOp;
use wafer_run::context::Context;
use wafer_run::types::WaferError;

// Re-export data types used by callers.
pub use crate::blocks::storage::service::{FolderInfo, ObjectInfo, ObjectList};
pub use crate::blocks::storage::service::ListOptions;

use super::{call_service, decode};

const BLOCK: &str = "wafer/storage";

// --- Wire-format types ---

#[derive(Serialize)]
struct PutReq<'a> {
    folder: &'a str,
    key: &'a str,
    data: &'a [u8],
    content_type: &'a str,
}

#[derive(Serialize)]
struct GetReq<'a> {
    folder: &'a str,
    key: &'a str,
}

#[derive(Deserialize)]
struct GetResp {
    data: Vec<u8>,
    info: ObjectInfo,
}

#[derive(Serialize)]
struct DeleteReq<'a> {
    folder: &'a str,
    key: &'a str,
}

#[derive(Serialize)]
struct ListReq<'a> {
    folder: &'a str,
    prefix: &'a str,
    limit: i64,
    offset: i64,
}

#[derive(Serialize)]
struct CreateFolderReq<'a> {
    name: &'a str,
    public: bool,
}

#[derive(Serialize)]
struct DeleteFolderReq<'a> {
    name: &'a str,
}

// --- Public API ---

/// Store an object.
pub fn put(
    ctx: &dyn Context,
    folder: &str,
    key: &str,
    data: &[u8],
    content_type: &str,
) -> Result<(), WaferError> {
    call_service(
        ctx,
        BLOCK,
        ServiceOp::STORAGE_PUT,
        &PutReq {
            folder,
            key,
            data,
            content_type,
        },
    )?;
    Ok(())
}

/// Retrieve an object and its metadata.
pub fn get(
    ctx: &dyn Context,
    folder: &str,
    key: &str,
) -> Result<(Vec<u8>, ObjectInfo), WaferError> {
    let data = call_service(ctx, BLOCK, ServiceOp::STORAGE_GET, &GetReq { folder, key })?;
    let resp: GetResp = decode(&data)?;
    Ok((resp.data, resp.info))
}

/// Delete an object.
pub fn delete(ctx: &dyn Context, folder: &str, key: &str) -> Result<(), WaferError> {
    call_service(
        ctx,
        BLOCK,
        ServiceOp::STORAGE_DELETE,
        &DeleteReq { folder, key },
    )?;
    Ok(())
}

/// List objects in a folder.
pub fn list(ctx: &dyn Context, folder: &str, opts: &ListOptions) -> Result<ObjectList, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::STORAGE_LIST,
        &ListReq {
            folder,
            prefix: &opts.prefix,
            limit: opts.limit,
            offset: opts.offset,
        },
    )?;
    decode(&data)
}

/// Create a storage folder.
pub fn create_folder(
    ctx: &dyn Context,
    name: &str,
    public: bool,
) -> Result<(), WaferError> {
    call_service(
        ctx,
        BLOCK,
        ServiceOp::STORAGE_CREATE_FOLDER,
        &CreateFolderReq { name, public },
    )?;
    Ok(())
}

/// Delete a storage folder and all its contents.
pub fn delete_folder(ctx: &dyn Context, name: &str) -> Result<(), WaferError> {
    call_service(
        ctx,
        BLOCK,
        ServiceOp::STORAGE_DELETE_FOLDER,
        &DeleteFolderReq { name },
    )?;
    Ok(())
}

/// List all storage folders.
pub fn list_folders(ctx: &dyn Context) -> Result<Vec<FolderInfo>, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::STORAGE_LIST_FOLDERS,
        &serde_json::json!({}),
    )?;
    decode(&data)
}
