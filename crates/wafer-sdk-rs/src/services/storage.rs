//! Storage service client — calls `wafer/storage` block via `call-block`.

use serde::{Deserialize, Serialize};

use crate::types::{Action, Message};
use crate::call_block;

/// Metadata about a stored object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectInfo {
    pub key: String,
    pub size: i64,
    pub content_type: String,
    pub last_modified: String,
}

/// A stored object: content bytes together with metadata.
#[derive(Debug, Clone)]
pub struct Object {
    pub data: Vec<u8>,
    pub info: ObjectInfo,
}

/// Storage error type.
#[derive(Debug, Clone)]
pub struct StorageError {
    pub kind: String,
    pub message: String,
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for StorageError {}

// --- Internal request types ---

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

#[derive(Deserialize)]
struct GetResp {
    data: Vec<u8>,
    info: ObjectInfo,
}

#[derive(Deserialize)]
struct ListResp {
    objects: Vec<ObjectInfo>,
}

// --- Helpers ---

fn make_msg(kind: &str, data: &impl Serialize) -> Message {
    Message::new(kind, serde_json::to_vec(data).unwrap_or_default())
}

fn call_storage(msg: &Message) -> Result<Vec<u8>, StorageError> {
    let result = call_block("wafer-run/storage", msg);
    match result.action {
        Action::Error => {
            let err_msg = result.error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown storage error".to_string());
            if err_msg.contains("not found") {
                Err(StorageError { kind: "not_found".into(), message: err_msg })
            } else {
                Err(StorageError { kind: "internal".into(), message: err_msg })
            }
        }
        _ => Ok(result.response.map(|r| r.data).unwrap_or_default()),
    }
}

fn call_storage_parse<T: serde::de::DeserializeOwned>(msg: &Message) -> Result<T, StorageError> {
    let data = call_storage(msg)?;
    serde_json::from_slice(&data).map_err(|e| StorageError {
        kind: "internal".into(),
        message: format!("failed to parse response: {e}"),
    })
}

// --- Public API ---

/// Store an object in a folder.
pub fn put(folder: &str, key: &str, data: &[u8], content_type: &str) -> Result<(), StorageError> {
    let msg = make_msg("storage.put", &PutReq { folder, key, data, content_type });
    call_storage(&msg)?;
    Ok(())
}

/// Retrieve an object and its metadata.
pub fn get(folder: &str, key: &str) -> Result<Object, StorageError> {
    let msg = make_msg("storage.get", &GetReq { folder, key });
    let resp: GetResp = call_storage_parse(&msg)?;
    Ok(Object {
        data: resp.data,
        info: resp.info,
    })
}

/// Delete an object.
pub fn delete(folder: &str, key: &str) -> Result<(), StorageError> {
    let msg = make_msg("storage.delete", &DeleteReq { folder, key });
    call_storage(&msg)?;
    Ok(())
}

/// List objects in a folder.
pub fn list(folder: &str, prefix: &str, limit: i64, offset: i64) -> Result<Vec<ObjectInfo>, StorageError> {
    let msg = make_msg("storage.list", &ListReq { folder, prefix, limit, offset });
    let resp: ListResp = call_storage_parse(&msg)?;
    Ok(resp.objects)
}
