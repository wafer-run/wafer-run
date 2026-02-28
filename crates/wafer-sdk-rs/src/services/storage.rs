//! Storage service client using WIT-generated imports.

use serde::{Deserialize, Serialize};

use crate::wafer::block_world::storage as wit;

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

fn convert_wit_error(e: wit::StorageError) -> StorageError {
    match e {
        wit::StorageError::NotFound => StorageError { kind: "not_found".into(), message: "object not found".into() },
        wit::StorageError::Internal => StorageError { kind: "internal".into(), message: "internal storage error".into() },
    }
}

/// Store an object in a folder.
pub fn put(folder: &str, key: &str, data: &[u8], content_type: &str) -> Result<(), StorageError> {
    wit::put(folder, key, data, content_type).map_err(convert_wit_error)
}

/// Retrieve an object and its metadata.
pub fn get(folder: &str, key: &str) -> Result<Object, StorageError> {
    wit::get(folder, key)
        .map(|(data, info)| Object {
            data,
            info: ObjectInfo {
                key: info.key,
                size: info.size,
                content_type: info.content_type,
                last_modified: info.last_modified,
            },
        })
        .map_err(convert_wit_error)
}

/// Delete an object.
pub fn delete(folder: &str, key: &str) -> Result<(), StorageError> {
    wit::delete(folder, key).map_err(convert_wit_error)
}

/// List objects in a folder.
pub fn list(folder: &str, prefix: &str, limit: i64, offset: i64) -> Result<Vec<ObjectInfo>, StorageError> {
    wit::list(folder, prefix, limit, offset)
        .map(|ol| ol.objects.into_iter().map(|o| ObjectInfo {
            key: o.key,
            size: o.size,
            content_type: o.content_type,
            last_modified: o.last_modified,
        }).collect())
        .map_err(convert_wit_error)
}
