use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("object not found")]
    NotFound,
    #[error("storage error: {0}")]
    Internal(String),
    #[error("{0}")]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

/// Service provides file/object storage operations organized by folders.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait StorageService: Send + Sync {
    /// Put stores an object in a folder.
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), StorageError>;

    /// Get retrieves an object and its metadata from a folder.
    async fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError>;

    /// Delete removes an object from a folder.
    async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError>;

    /// List returns objects in a folder with optional pagination.
    async fn list(&self, folder: &str, opts: &ListOptions) -> Result<ObjectList, StorageError>;

    /// CreateFolder creates a new storage folder.
    async fn create_folder(&self, name: &str, public: bool) -> Result<(), StorageError>;

    /// DeleteFolder removes a storage folder and all its contents.
    async fn delete_folder(&self, name: &str) -> Result<(), StorageError>;

    /// ListFolders returns all storage folders.
    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError>;
}

/// ObjectInfo contains metadata about a stored object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectInfo {
    pub key: String,
    pub size: i64,
    pub content_type: String,
    pub last_modified: DateTime<Utc>,
}

/// ObjectList represents a paginated list of objects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectList {
    pub objects: Vec<ObjectInfo>,
    pub total_count: i64,
}

/// FolderInfo contains metadata about a storage folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderInfo {
    pub name: String,
    pub public: bool,
    pub created_at: DateTime<Utc>,
}

/// ListOptions configures a List query for objects.
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    pub prefix: String,
    pub limit: i64,
    pub offset: i64,
}
