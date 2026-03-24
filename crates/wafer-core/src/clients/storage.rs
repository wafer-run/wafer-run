use serde::{Deserialize, Serialize};

use wafer_run::common::ServiceOp;
#[cfg(not(feature = "wasm-component"))]
use wafer_run::context::Context;
use wafer_run::types::WaferError;

// Re-export data types used by callers.
pub use crate::interfaces::storage::service::{FolderInfo, ObjectInfo, ObjectList};
pub use crate::interfaces::storage::service::ListOptions;

use super::{call_service, decode};

const BLOCK: &str = "wafer-run/storage";

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

// ===========================================================================
// Public API — native async
// ===========================================================================

#[cfg(not(feature = "wasm-component"))]
pub async fn put(
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
    ).await?;
    Ok(())
}

#[cfg(not(feature = "wasm-component"))]
pub async fn get(
    ctx: &dyn Context,
    folder: &str,
    key: &str,
) -> Result<(Vec<u8>, ObjectInfo), WaferError> {
    let data = call_service(ctx, BLOCK, ServiceOp::STORAGE_GET, &GetReq { folder, key }).await?;
    let resp: GetResp = decode(&data)?;
    Ok((resp.data, resp.info))
}

#[cfg(not(feature = "wasm-component"))]
pub async fn delete(ctx: &dyn Context, folder: &str, key: &str) -> Result<(), WaferError> {
    call_service(
        ctx,
        BLOCK,
        ServiceOp::STORAGE_DELETE,
        &DeleteReq { folder, key },
    ).await?;
    Ok(())
}

#[cfg(not(feature = "wasm-component"))]
pub async fn list(ctx: &dyn Context, folder: &str, opts: &ListOptions) -> Result<ObjectList, WaferError> {
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
    ).await?;
    decode(&data)
}

#[cfg(not(feature = "wasm-component"))]
pub async fn create_folder(
    ctx: &dyn Context,
    name: &str,
    public: bool,
) -> Result<(), WaferError> {
    call_service(
        ctx,
        BLOCK,
        ServiceOp::STORAGE_CREATE_FOLDER,
        &CreateFolderReq { name, public },
    ).await?;
    Ok(())
}

#[cfg(not(feature = "wasm-component"))]
pub async fn delete_folder(ctx: &dyn Context, name: &str) -> Result<(), WaferError> {
    call_service(
        ctx,
        BLOCK,
        ServiceOp::STORAGE_DELETE_FOLDER,
        &DeleteFolderReq { name },
    ).await?;
    Ok(())
}

#[cfg(not(feature = "wasm-component"))]
pub async fn list_folders(ctx: &dyn Context) -> Result<Vec<FolderInfo>, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::STORAGE_LIST_FOLDERS,
        &serde_json::json!({}),
    ).await?;
    decode(&data)
}

// ===========================================================================
// Public API — WASM sync
// ===========================================================================

#[cfg(feature = "wasm-component")]
pub fn put(
    folder: &str,
    key: &str,
    data: &[u8],
    content_type: &str,
) -> Result<(), WaferError> {
    call_service(
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

#[cfg(feature = "wasm-component")]
pub fn get(
    folder: &str,
    key: &str,
) -> Result<(Vec<u8>, ObjectInfo), WaferError> {
    let data = call_service(BLOCK, ServiceOp::STORAGE_GET, &GetReq { folder, key })?;
    let resp: GetResp = decode(&data)?;
    Ok((resp.data, resp.info))
}

#[cfg(feature = "wasm-component")]
pub fn delete(folder: &str, key: &str) -> Result<(), WaferError> {
    call_service(
        BLOCK,
        ServiceOp::STORAGE_DELETE,
        &DeleteReq { folder, key },
    )?;
    Ok(())
}

#[cfg(feature = "wasm-component")]
pub fn list(folder: &str, opts: &ListOptions) -> Result<ObjectList, WaferError> {
    let data = call_service(
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

#[cfg(feature = "wasm-component")]
pub fn create_folder(
    name: &str,
    public: bool,
) -> Result<(), WaferError> {
    call_service(
        BLOCK,
        ServiceOp::STORAGE_CREATE_FOLDER,
        &CreateFolderReq { name, public },
    )?;
    Ok(())
}

#[cfg(feature = "wasm-component")]
pub fn delete_folder(name: &str) -> Result<(), WaferError> {
    call_service(
        BLOCK,
        ServiceOp::STORAGE_DELETE_FOLDER,
        &DeleteFolderReq { name },
    )?;
    Ok(())
}

#[cfg(feature = "wasm-component")]
pub fn list_folders() -> Result<Vec<FolderInfo>, WaferError> {
    let data = call_service(
        BLOCK,
        ServiceOp::STORAGE_LIST_FOLDERS,
        &serde_json::json!({}),
    )?;
    decode(&data)
}
