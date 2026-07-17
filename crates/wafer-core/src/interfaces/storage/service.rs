use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use wafer_block::{InputStream, OutputStream};
use wafer_block_macro::wafer_async_trait;

/// Errors returned by [`StorageService`] operations.
#[derive(Error, Debug)]
pub enum StorageError {
    /// No object exists at the requested folder/key.
    #[error("object not found")]
    NotFound,
    /// Backend-internal failure.
    #[error("storage error: {0}")]
    Internal(String),
    /// Wrapped foreign error from a backend driver.
    #[error("{0}")]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

/// Service provides file/object storage operations organized by folders.
#[wafer_async_trait]
pub trait StorageService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// Put stores an object in a folder.
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), StorageError>;

    /// Streaming variant of [`put`](Self::put): store an object whose body
    /// arrives as an [`InputStream`] of byte chunks instead of a single
    /// fully-buffered slice.
    ///
    /// The default collapses the stream to bytes and forwards to
    /// [`put`](Self::put), so existing backends keep working unchanged.
    /// Backends whose driver accepts an incremental write (a filesystem
    /// append, a multipart upload) SHOULD override this to avoid holding the
    /// whole object in memory — the buffering here is exactly what the
    /// streaming request path exists to avoid.
    async fn put_streaming(
        &self,
        folder: &str,
        key: &str,
        data: InputStream,
        content_type: &str,
    ) -> Result<(), StorageError> {
        let bytes = data.collect_to_bytes().await;
        self.put(folder, key, &bytes, content_type).await
    }

    /// Get retrieves an object and its metadata from a folder.
    async fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError>;

    /// Streaming variant of [`get`](Self::get): retrieve an object as an
    /// [`OutputStream`] of body chunks paired with its [`ObjectInfo`].
    ///
    /// The default calls [`get`](Self::get) and wraps the buffered body as a
    /// single-chunk stream, so existing backends keep working unchanged.
    /// Backends whose driver exposes a chunked or byte-range read (a
    /// filesystem read, an S3 `GetObject` body stream) SHOULD override this to
    /// stream the body without buffering it whole.
    async fn get_streaming(
        &self,
        folder: &str,
        key: &str,
    ) -> Result<(OutputStream, ObjectInfo), StorageError> {
        let (data, info) = self.get(folder, key).await?;
        Ok((OutputStream::respond(data), info))
    }

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
    /// Object key (path within its folder).
    pub key: String,
    /// Size of the object body in bytes.
    pub size: i64,
    /// Content-Type recorded at upload time.
    pub content_type: String,
    /// Last-modified timestamp from the backend.
    pub last_modified: DateTime<Utc>,
}

/// ObjectList represents a paginated list of objects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectList {
    /// Objects in the current page.
    pub objects: Vec<ObjectInfo>,
    /// Total objects matching the query across all pages.
    ///
    /// Backends where an exact total would require walking the entire
    /// keyspace (e.g. S3) may return a lower bound when a `limit` was
    /// given and the listing stopped early. The bound is always strictly
    /// greater than `offset + limit` when more objects exist, so
    /// `total_count > offset + limit` remains a correct has-more-pages
    /// check.
    pub total_count: i64,
}

/// FolderInfo contains metadata about a storage folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderInfo {
    /// Folder / bucket name.
    pub name: String,
    /// Whether the folder grants public read access.
    pub public: bool,
    /// Creation timestamp from the backend.
    pub created_at: DateTime<Utc>,
}

/// ListOptions configures a List query for objects.
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    /// Restrict results to keys starting with this prefix (empty = no filter).
    pub prefix: String,
    /// Maximum number of objects to return (0 = backend default).
    pub limit: i64,
    /// Number of objects to skip for pagination.
    pub offset: i64,
}
