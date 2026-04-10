use serde::{Deserialize, Serialize};

use wafer_block::common::ServiceOp;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
use wafer_block::WaferError;

// Re-export data types used by callers.
pub use crate::interfaces::storage::service::ListOptions;
pub use crate::interfaces::storage::service::{FolderInfo, ObjectInfo, ObjectList};

use super::{call_service, decode, dual_api, svc};

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
// Public API — generated as async (native) or sync (wasm-component)
// ===========================================================================

dual_api! {
    pub fn put(ctx, folder: &str, key: &str, data: &[u8], content_type: &str) -> Result<(), WaferError> {
        svc!(ctx, BLOCK, ServiceOp::STORAGE_PUT, &PutReq { folder, key, data, content_type }, None::<&str>, true, Some("storage"))?;
        Ok(())
    }

    pub fn get(ctx, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), WaferError> {
        let data = svc!(ctx, BLOCK, ServiceOp::STORAGE_GET, &GetReq { folder, key }, None::<&str>, false, Some("storage"))?;
        let resp: GetResp = decode(&data)?;
        Ok((resp.data, resp.info))
    }

    pub fn delete(ctx, folder: &str, key: &str) -> Result<(), WaferError> {
        svc!(ctx, BLOCK, ServiceOp::STORAGE_DELETE, &DeleteReq { folder, key }, None::<&str>, true, Some("storage"))?;
        Ok(())
    }

    pub fn list(ctx, folder: &str, opts: &ListOptions) -> Result<ObjectList, WaferError> {
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::STORAGE_LIST,
            &ListReq { folder, prefix: &opts.prefix, limit: opts.limit, offset: opts.offset },
            None::<&str>,
            false,
            Some("storage")
        )?;
        decode(&data)
    }

    pub fn create_folder(ctx, name: &str, public: bool) -> Result<(), WaferError> {
        svc!(ctx, BLOCK, ServiceOp::STORAGE_CREATE_FOLDER, &CreateFolderReq { name, public }, Some(name), true, Some("storage"))?;
        Ok(())
    }

    pub fn delete_folder(ctx, name: &str) -> Result<(), WaferError> {
        svc!(ctx, BLOCK, ServiceOp::STORAGE_DELETE_FOLDER, &DeleteFolderReq { name }, Some(name), true, Some("storage"))?;
        Ok(())
    }

    pub fn list_folders(ctx,) -> Result<Vec<FolderInfo>, WaferError> {
        let data = svc!(ctx, BLOCK, ServiceOp::STORAGE_LIST_FOLDERS, &serde_json::json!({}), None, false, Some("storage"))?;
        decode(&data)
    }
}
