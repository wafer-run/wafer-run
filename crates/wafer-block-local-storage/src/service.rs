use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use chrono::Utc;
use futures::StreamExt;
use wafer_block::{common::ErrorCode, InputStream, OutputStream, WaferError};
use wafer_block_macro::wafer_async_trait;
use wafer_core::interfaces::storage::service::*;

/// Refuse a read of a file larger than [`DEFAULT_MAX_OBJECT_BYTES`].
fn check_object_size(path: &Path, len: u64) -> Result<(), StorageError> {
    if len > DEFAULT_MAX_OBJECT_BYTES {
        return Err(StorageError::TooLarge(format!(
            "file {path:?} is {len} bytes, exceeds limit of {DEFAULT_MAX_OBJECT_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Lexically normalize an absolute path: resolve `.` and `..` components
/// without consulting the filesystem.
///
/// Returns `None` if `..` would escape above the path root (e.g. trying to
/// pop a component off `/`). The result preserves the path's existence-
/// agnostic semantics — used by [`LocalStorageService::validate_path`] for
/// inputs that haven't been written yet.
fn normalize_lexical(path: &Path) -> Option<PathBuf> {
    let mut out: Vec<Component<'_>> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {
                // Skip `.`
            }
            Component::ParentDir => {
                // Pop the last *normal* component. If the last entry is the
                // root, popping would escape above the filesystem root —
                // treat as traversal.
                match out.last() {
                    Some(Component::Normal(_)) => {
                        out.pop();
                    }
                    Some(Component::RootDir) | Some(Component::Prefix(_)) | None => {
                        return None;
                    }
                    // ParentDir / CurDir can't appear in `out` because we
                    // never push them.
                    Some(_) => return None,
                }
            }
            other => out.push(other),
        }
    }
    Some(out.iter().map(|c| c.as_os_str()).collect())
}

/// Name of the directory, directly under the storage root, where writes are
/// staged before they are renamed onto their key. It is not an object folder:
/// `list` and `list_folders` never show it, no request may address a path in
/// it, and [`LocalStorageService::new`] empties it.
const STAGING_DIR: &str = ".wafer-staging";

/// Whether `path` is the staging directory under any spelling the filesystem
/// resolves to it: the name compared ASCII-case-insensitively, and — for the
/// folds a case-insensitive filesystem applies beyond ASCII, or a link — the
/// filesystem's own answer on whether it is the same directory.
fn is_staging(path: &Path, staging: &Path) -> bool {
    let named = path.file_name().is_some_and(|name| {
        name.to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(STAGING_DIR))
    });
    named || same_file::is_same_file(path, staging).unwrap_or(false)
}

/// Local filesystem implementation of StorageService.
///
/// One process owns a storage root: [`new`](Self::new) deletes every staged
/// write it finds, which would fail the in-flight writes of a second process
/// sharing the root. Every object folder must be on the root's filesystem,
/// because a write is renamed there from the root's staging directory; a
/// folder mounted or symlinked onto another filesystem fails every write with
/// an error that says so.
pub struct LocalStorageService {
    root: PathBuf,
}

impl LocalStorageService {
    /// Construct a service rooted at `root`, creating the directory tree if it
    /// does not yet exist. All subsequent reads/writes are confined to this
    /// root via [`Self::validate_path`].
    ///
    /// Staged writes left behind by a process that stopped mid-write are
    /// deleted here (see [`STAGING_DIR`]).
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root = root.into();
        fs::create_dir_all(&root)
            .map_err(|e| StorageError::Internal(format!("create storage root {root:?}: {e}")))?;
        // Canonicalize so the root is always absolute. Without this, a
        // caller-supplied relative root (e.g. an application's default
        // `data/storage`) makes `object_path(folder, key)` return a
        // relative path. `validate_path` then re-joins it onto the
        // canonicalized root, producing a doubled-prefix path like
        // `/cwd/data/storage/data/storage/folder/key` that `fs::write`
        // can't find. Every op (`put`/`get`/`delete`/`create_folder`/
        // `delete_folder`/`list`) uses the `validate_path` return value
        // directly rather than the raw joined path.
        let root = root.canonicalize().map_err(|e| {
            StorageError::Internal(format!("canonicalize storage root {root:?}: {e}"))
        })?;
        let staging = root.join(STAGING_DIR);
        match fs::remove_dir_all(&staging) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(StorageError::Internal(format!(
                    "clear staging directory {staging:?}: {e}"
                )))
            }
        }
        fs::create_dir_all(&staging).map_err(|e| {
            StorageError::Internal(format!("create staging directory {staging:?}: {e}"))
        })?;
        Ok(Self { root })
    }

    fn staging_path(&self) -> PathBuf {
        self.root.join(STAGING_DIR)
    }

    fn folder_path(&self, folder: &str) -> PathBuf {
        self.root.join(folder)
    }

    fn object_path(&self, folder: &str, key: &str) -> PathBuf {
        self.root.join(folder).join(key)
    }

    /// Validate that a resolved path stays within the storage root.
    ///
    /// Prevents path traversal attacks via `../` in folder or key names by
    /// normalizing the path components *lexically* (without touching the
    /// filesystem) and then comparing against the canonicalized root.
    ///
    /// Lexical normalization is critical: `path.canonicalize()` only works
    /// when the path exists, so for `put`/`create` operations we have to
    /// resolve `.` / `..` components ourselves — falling back to "just use
    /// the raw path" (as the previous implementation did when no parent
    /// existed) would bypass traversal checks entirely (SEC-024).
    fn validate_path(&self, path: &Path) -> Result<PathBuf, StorageError> {
        // Canonicalize the root (always exists after `new`)
        let canon_root = self.root.canonicalize().map_err(|e| {
            StorageError::Internal(format!("canonicalize root {:?}: {}", self.root, e))
        })?;

        // Resolve `path` to an absolute, lexically-normalized form.
        // Start from canon_root if the input is relative.
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            canon_root.join(path)
        };

        let normalized = normalize_lexical(&absolute).ok_or_else(|| {
            StorageError::InvalidArgument(
                "path traversal: resolved path escapes storage root".to_string(),
            )
        })?;

        if !normalized.starts_with(&canon_root) {
            return Err(StorageError::InvalidArgument(
                "path traversal: resolved path escapes storage root".to_string(),
            ));
        }
        let first = normalized
            .strip_prefix(&canon_root)
            .ok()
            .and_then(|rel| rel.components().next());
        if first.is_some_and(|first| {
            is_staging(
                &canon_root.join(first.as_os_str()),
                &canon_root.join(STAGING_DIR),
            )
        }) {
            return Err(StorageError::InvalidArgument(format!(
                "{STAGING_DIR} is reserved for staged writes"
            )));
        }
        Ok(normalized)
    }

    fn guess_content_type(key: &str) -> String {
        wafer_core::mime::mime_for_ext(Path::new(key)).to_string()
    }

    /// Atomically write an object at `path`: `fill` a temp file in the root's
    /// [`STAGING_DIR`], then `rename` it onto `path` on success. A reader
    /// therefore never observes a half-written object at the live key, and
    /// `list` never shows the temp file — a failure (including a `fill` that
    /// returns `Err`) leaves the previous object (or nothing) in place and the
    /// temp file is removed. POSIX `rename` within one filesystem is atomic.
    ///
    /// Durable as well as atomic: the temp file is `fsync`ed before the
    /// rename and, on Unix, the key's directory after it, so a power loss
    /// leaves the previous object or the new one, never an empty file.
    ///
    /// Shared by both `put` (buffered) and `put_streaming` so the atomicity
    /// guarantee cannot drift between them.
    async fn atomic_write<F, Fut>(&self, path: &Path, fill: F) -> Result<(), StorageError>
    where
        F: FnOnce(tokio::fs::File) -> Fut,
        Fut: std::future::Future<Output = Result<tokio::fs::File, StorageError>>,
    {
        use tokio::io::AsyncWriteExt;

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::Internal(format!("create dirs for {path:?}: {e}")))?;
        }

        // Re-created on every write, so a write still works after something
        // outside this service removed the directory.
        let staging = self.staging_path();
        tokio::fs::create_dir_all(&staging).await.map_err(|e| {
            StorageError::Internal(format!("create staging directory {staging:?}: {e}"))
        })?;
        let tmp = staged_temp_path(&staging);

        // Fill + flush the temp file; on any error remove it and propagate.
        let filled = async {
            let file = tokio::fs::File::create(&tmp)
                .await
                .map_err(|e| StorageError::Internal(format!("create temp {tmp:?}: {e}")))?;
            let mut file = fill(file).await?;
            file.flush()
                .await
                .map_err(|e| StorageError::Internal(format!("flush temp {tmp:?}: {e}")))?;
            file.sync_all()
                .await
                .map_err(|e| StorageError::Internal(format!("fsync temp {tmp:?}: {e}")))?;
            Ok::<(), StorageError>(())
        }
        .await;

        if let Err(e) = filled {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }

        // Commit: atomically swing the name onto the final path.
        if let Err(e) = tokio::fs::rename(&tmp, path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            if e.kind() == std::io::ErrorKind::CrossesDevices {
                return Err(StorageError::Internal(format!(
                    "cannot store {path:?}: its folder is on a different filesystem than the \
                     storage root {:?} (a mount point or a symlink under the root); \
                     LocalStorageService needs every folder on the root's filesystem",
                    self.root
                )));
            }
            return Err(StorageError::Internal(format!(
                "rename {tmp:?} -> {path:?}: {e}"
            )));
        }
        // Make the rename itself durable. Unix only: a directory cannot be
        // opened as a file to sync it on Windows, where NTFS journals the
        // rename. The object is already in place if this fails.
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            let synced = async { tokio::fs::File::open(parent).await?.sync_all().await }.await;
            if let Err(e) = synced {
                return Err(StorageError::Internal(format!(
                    "fsync directory {parent:?} after writing {path:?} (the object is in \
                     place but may not survive a power loss): {e}"
                )));
            }
        }
        Ok(())
    }
}

/// Map a `spawn_blocking` join failure (panicked or cancelled walk task).
fn join_err(e: &tokio::task::JoinError) -> StorageError {
    StorageError::Internal(format!("storage walk task failed: {e}"))
}

/// Encode an object key as this backend's opaque list cursor.
///
/// URL-safe unpadded base64 keeps the token opaque to callers (they must
/// never parse it) and safe to round-trip through the wire / URLs. Decoded
/// back to the key by [`decode_cursor`].
fn encode_cursor(key: &str) -> String {
    use base64ct::{Base64UrlUnpadded, Encoding};
    Base64UrlUnpadded::encode_string(key.as_bytes())
}

/// Decode an opaque list cursor back to the object key it was minted from.
/// A cursor that isn't valid base64 / UTF-8 is a client error, surfaced as
/// [`StorageError::InvalidArgument`].
fn decode_cursor(cursor: &str) -> Result<String, StorageError> {
    use base64ct::{Base64UrlUnpadded, Encoding};
    let bytes = Base64UrlUnpadded::decode_vec(cursor)
        .map_err(|e| StorageError::InvalidArgument(format!("invalid storage list cursor: {e}")))?;
    String::from_utf8(bytes).map_err(|e| {
        StorageError::InvalidArgument(format!("invalid storage list cursor (not utf-8): {e}"))
    })
}

/// Resolve a cursor to the start index into the sorted `objects` slice.
///
/// An empty cursor means "before the first object" → index 0. Otherwise the
/// scan resumes strictly AFTER the key the cursor encodes: with `objects`
/// sorted ascending by key, that is the first index whose key is greater than
/// the cursor key. A cursor key that no longer exists (its object was deleted
/// between pages) still resolves correctly — the resume point is the next
/// surviving key, so no object is skipped or repeated.
fn cursor_start_index(objects: &[ObjectInfo], cursor: &str) -> Result<usize, StorageError> {
    if cursor.is_empty() {
        return Ok(0);
    }
    let last_key = decode_cursor(cursor)?;
    Ok(objects.partition_point(|o| o.key <= last_key))
}

/// Map a metadata probe to the existence semantics the sync code had:
/// missing file → `NotFound`, any other error → `Internal`.
fn metadata_or_not_found(
    path: &Path,
    res: std::io::Result<std::fs::Metadata>,
) -> Result<std::fs::Metadata, StorageError> {
    res.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            StorageError::NotFound
        } else {
            StorageError::Internal(format!("metadata {path:?}: {e}"))
        }
    })
}

/// Monotonic per-process counter making temp-file names unique even when two
/// writes begin within the same nanosecond.
static TEMP_WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Build a unique temp path in `staging` (the root's [`STAGING_DIR`]). The
/// name carries the pid plus a monotonic sequence number so concurrent
/// writers never collide on the temp file.
fn staged_temp_path(staging: &Path) -> PathBuf {
    let seq = TEMP_WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    staging.join(format!("{pid}.{seq}.tmp"))
}

#[wafer_async_trait]
impl StorageService for LocalStorageService {
    /// Buffered write, made atomic via [`Self::atomic_write`]: the bytes land
    /// in a temp file that is renamed onto `key` only on success, so a reader
    /// never observes a partially written object (PERF-02: the executor thread
    /// only awaits; no payload copy into a `spawn_blocking` closure).
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        _content_type: &str,
    ) -> Result<(), StorageError> {
        let path = self.validate_path(&self.object_path(folder, key))?;
        let err_path = path.clone();
        self.atomic_write(&path, move |mut file| async move {
            use tokio::io::AsyncWriteExt;
            file.write_all(data)
                .await
                .map_err(|e| StorageError::Internal(format!("write {err_path:?}: {e}")))?;
            Ok(file)
        })
        .await
    }

    /// Streaming write: chunks are written to a temp file as they arrive and
    /// the temp file is renamed onto `key` only after the stream ends cleanly,
    /// so a reader never observes a partial object — same atomicity as
    /// [`put`](Self::put) via [`Self::atomic_write`], without holding the whole
    /// object in memory. A stream that ends in an error returns
    /// [`StorageError::Body`] and the temp file is removed: the key keeps its
    /// previous object, or stays absent.
    async fn put_streaming(
        &self,
        folder: &str,
        key: &str,
        mut data: InputStream,
        _content_type: &str,
    ) -> Result<(), StorageError> {
        let path = self.validate_path(&self.object_path(folder, key))?;
        let err_path = path.clone();
        self.atomic_write(&path, move |mut file| async move {
            use tokio::io::AsyncWriteExt;
            while let Some(chunk) = data.next().await {
                let chunk = chunk.map_err(StorageError::Body)?;
                file.write_all(&chunk)
                    .await
                    .map_err(|e| StorageError::Internal(format!("write {err_path:?}: {e}")))?;
            }
            Ok(file)
        })
        .await
    }

    async fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        let path = self.validate_path(&self.object_path(folder, key))?;
        let metadata = metadata_or_not_found(&path, tokio::fs::metadata(&path).await)?;

        // Refuse a file past the shared read cap rather than reading it whole.
        check_object_size(&path, metadata.len())?;

        let data = tokio::fs::read(&path)
            .await
            .map_err(|e| StorageError::Internal(format!("read {path:?}: {e}")))?;

        let last_modified = metadata
            .modified()
            .map_or_else(|_| Utc::now(), chrono::DateTime::<Utc>::from);

        let info = ObjectInfo {
            key: key.to_string(),
            size: data.len() as i64,
            content_type: Self::guess_content_type(key),
            last_modified,
        };

        Ok((data, info))
    }

    /// Streams the file from disk in fixed-size chunks through an
    /// [`OutputStream`] producer, so a large object is never read whole into
    /// memory (the buffered default reads the entire file first).
    ///
    /// `ObjectInfo.size` and the streamed bytes come from the SAME open file
    /// handle: the file is opened first, then `fstat`'d via that handle, then
    /// read from that handle. A concurrent writer therefore cannot make the
    /// advertised size disagree with the bytes actually streamed — a consumer
    /// that sets `Content-Length: size` and then forwards the stream stays
    /// consistent (TOCTOU-free).
    async fn get_streaming(
        &self,
        folder: &str,
        key: &str,
    ) -> Result<(OutputStream, ObjectInfo), StorageError> {
        let path = self.validate_path(&self.object_path(folder, key))?;

        // Open first, then stat *that* handle — one snapshot backs both the
        // advertised size and the streamed bytes.
        let file = tokio::fs::File::open(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound
            } else {
                StorageError::Internal(format!("open {path:?}: {e}"))
            }
        })?;
        let metadata = file
            .metadata()
            .await
            .map_err(|e| StorageError::Internal(format!("metadata {path:?}: {e}")))?;

        // Same guard as `get`: refuse absurdly large files up front.
        check_object_size(&path, metadata.len())?;

        let last_modified = metadata
            .modified()
            .map_or_else(|_| Utc::now(), chrono::DateTime::<Utc>::from);
        let info = ObjectInfo {
            key: key.to_string(),
            size: metadata.len() as i64,
            content_type: Self::guess_content_type(key),
            last_modified,
        };

        let stream = OutputStream::from_producer(move |sink, cancel| async move {
            use tokio::io::AsyncReadExt;

            let mut file = file;
            // 64 KiB read window — the chunk size the streaming path emits.
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let read = tokio::select! {
                    biased;
                    // Consumer dropped the stream mid-read — abort promptly
                    // rather than blocking until the read resolves.
                    () = cancel.cancelled() => return,
                    read = file.read(&mut buf) => read,
                };
                match read {
                    Ok(0) => break,
                    Ok(n) => {
                        if sink.send_chunk(buf[..n].to_vec()).await.is_err() {
                            // Consumer dropped the stream — stop reading.
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = sink
                            .error(WaferError::new(
                                ErrorCode::Internal,
                                format!("read {path:?}: {e}"),
                            ))
                            .await;
                        return;
                    }
                }
            }
            let _ = sink.complete(vec![]).await;
        });

        Ok((stream, info))
    }

    async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
        let path = self.validate_path(&self.object_path(folder, key))?;
        metadata_or_not_found(&path, tokio::fs::metadata(&path).await)?;
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| StorageError::Internal(format!("delete {path:?}: {e}")))
    }

    /// The recursive walk is one `spawn_blocking` hop (many small syscalls —
    /// cheaper as a single blocking task than as per-entry async calls).
    ///
    /// Supports both offset pagination (`opts.offset`/`opts.limit`) and
    /// cursor pagination (`opts.cursor`). The full key list is sorted so both
    /// are stable — `read_dir` yields entries in an unspecified order, which a
    /// resumable cursor cannot rely on. Cursor takes precedence over offset
    /// (documented on [`ListOptions`]); the opaque cursor is the base64-encoded
    /// key of the last object returned on the previous page, and the scan
    /// resumes strictly after it. `total_count` is always the exact match
    /// count (the walk sees every object regardless of paging mode).
    async fn list(&self, folder: &str, opts: &ListOptions) -> Result<ObjectList, StorageError> {
        let dir = self.validate_path(&self.folder_path(folder))?;
        let staging = self.staging_path();
        let prefix = opts.prefix.clone();
        let offset = opts.offset as usize;
        let limit = opts.limit;
        let cursor = opts.cursor.clone();

        tokio::task::spawn_blocking(move || {
            if !dir.exists() {
                return Ok(ObjectList::default());
            }

            let mut objects = Vec::new();
            Self::list_recursive(&dir, &dir, &staging, &prefix, &mut objects)?;

            // Sort by key so offset and cursor pagination are both stable and
            // deterministic across runs and filesystems.
            objects.sort_by(|a, b| a.key.cmp(&b.key));

            let total_count = objects.len() as i64;

            let page_limit = if limit > 0 {
                limit as usize
            } else {
                objects.len()
            };

            // Cursor mode takes precedence over offset. An empty cursor starts
            // at the first object; a non-empty one resumes strictly after the
            // key it encodes.
            let start = match &cursor {
                Some(token) => cursor_start_index(&objects, token)?,
                None => offset.min(objects.len()),
            };
            let end = start.saturating_add(page_limit).min(objects.len());
            let window: Vec<ObjectInfo> = objects[start..end].to_vec();

            // Emit a next_cursor only in cursor mode, and only when more
            // objects follow this page. Offset callers always get `None`, so
            // their behavior is unchanged.
            let next_cursor = if cursor.is_some() && end < objects.len() {
                window.last().map(|o| encode_cursor(&o.key))
            } else {
                None
            };

            Ok(ObjectList {
                objects: window,
                total_count,
                next_cursor,
            })
        })
        .await
        .map_err(|e| join_err(&e))?
    }

    async fn create_folder(&self, name: &str, _public: bool) -> Result<(), StorageError> {
        let path = self.validate_path(&self.folder_path(name))?;
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|e| StorageError::Internal(format!("create folder {path:?}: {e}")))?;
        Ok(())
    }

    async fn delete_folder(&self, name: &str) -> Result<(), StorageError> {
        let path = self.validate_path(&self.folder_path(name))?;
        metadata_or_not_found(&path, tokio::fs::metadata(&path).await)?;
        tokio::fs::remove_dir_all(&path)
            .await
            .map_err(|e| StorageError::Internal(format!("delete folder {path:?}: {e}")))
    }

    /// Directory scan runs as one `spawn_blocking` hop, like [`Self::list`].
    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        let root = self.root.clone();
        let staging = self.staging_path();
        tokio::task::spawn_blocking(move || {
            let mut folders = Vec::new();
            let entries = fs::read_dir(&root)
                .map_err(|e| StorageError::Internal(format!("read dir {root:?}: {e}")))?;

            for entry in entries {
                let entry =
                    entry.map_err(|e| StorageError::Internal(format!("read entry: {e}")))?;
                let metadata = entry
                    .metadata()
                    .map_err(|e| StorageError::Internal(format!("metadata: {e}")))?;
                if metadata.is_dir() && !is_staging(&entry.path(), &staging) {
                    let created_at = metadata
                        .created()
                        .map_or_else(|_| Utc::now(), chrono::DateTime::<Utc>::from);
                    folders.push(FolderInfo {
                        name: entry.file_name().to_string_lossy().to_string(),
                        public: false,
                        created_at,
                    });
                }
            }

            Ok(folders)
        })
        .await
        .map_err(|e| join_err(&e))?
    }
}

impl LocalStorageService {
    /// Collect every object under `dir`, keyed relative to `base`, skipping
    /// the `staging` directory (only reachable when `dir` is the root).
    fn list_recursive(
        base: &Path,
        dir: &Path,
        staging: &Path,
        prefix: &str,
        objects: &mut Vec<ObjectInfo>,
    ) -> Result<(), StorageError> {
        let entries = fs::read_dir(dir)
            .map_err(|e| StorageError::Internal(format!("read dir {dir:?}: {e}")))?;

        for entry in entries {
            let entry = entry.map_err(|e| StorageError::Internal(format!("read entry: {e}")))?;
            let path = entry.path();
            let metadata = entry
                .metadata()
                .map_err(|e| StorageError::Internal(format!("metadata: {e}")))?;

            // Only a directory, or a link that may resolve to one, can be the
            // staging directory; files skip the identity check's open.
            let dir_like = metadata.is_dir() || metadata.file_type().is_symlink();
            if dir_like && is_staging(&path, staging) {
                continue;
            }
            if metadata.is_dir() {
                Self::list_recursive(base, &path, staging, prefix, objects)?;
            } else {
                let key = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();

                if !prefix.is_empty() && !key.starts_with(prefix) {
                    continue;
                }

                let last_modified = metadata
                    .modified()
                    .map_or_else(|_| Utc::now(), chrono::DateTime::<Utc>::from);

                objects.push(ObjectInfo {
                    key: key.clone(),
                    size: metadata.len() as i64,
                    content_type: Self::guess_content_type(&key),
                    last_modified,
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_resolves_curdir_and_parentdir() {
        let p = Path::new("/root/a/./b/../c");
        assert_eq!(normalize_lexical(p), Some(PathBuf::from("/root/a/c")));
    }

    #[test]
    fn normalize_rejects_escape_above_root() {
        // `..` past the filesystem root must be rejected.
        assert_eq!(normalize_lexical(Path::new("/../etc")), None);
        assert_eq!(normalize_lexical(Path::new("/a/../../etc")), None);
    }

    #[test]
    fn normalize_no_op_on_clean_path() {
        let p = Path::new("/root/storage/folder/key");
        assert_eq!(
            normalize_lexical(p),
            Some(PathBuf::from("/root/storage/folder/key"))
        );
    }

    /// Regression for the SEC-024 bug: `validate_path` used to fall through
    /// to "just return the raw path" when the parent didn't exist, so a
    /// traversal payload would pass undetected. The helper must reject
    /// even when nothing on the filesystem has been created yet.
    #[test]
    fn validate_path_rejects_traversal_when_parent_missing() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");

        // `<root>/folder/../../etc/passwd` — neither `folder` nor the
        // resolved parent exists. Must still be rejected.
        let evil = svc
            .root
            .join("folder")
            .join("..")
            .join("..")
            .join("etc")
            .join("passwd");
        let err = svc.validate_path(&evil).expect_err("must reject traversal");
        match err {
            StorageError::InvalidArgument(msg) => assert!(
                msg.contains("path traversal"),
                "expected traversal error, got: {msg}"
            ),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn validate_path_accepts_path_inside_root_even_when_missing() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");

        // Inside root, just hasn't been created yet — should pass.
        let inside = svc.root.join("new_folder").join("new_key");
        let ok = svc.validate_path(&inside).expect("should accept");
        assert!(ok.starts_with(svc.root.canonicalize().unwrap()));
    }

    /// Regression: `put` used to call `fs::create_dir_all` on the raw
    /// joined path BEFORE `validate_path`. `create_dir_all` resolves `..`
    /// components at the syscall level as it walks up creating ancestors,
    /// so a traversal key materialized directories *outside* the storage
    /// root before the write was ever refused. Validation must happen
    /// first, and `create_dir_all` must only ever run on its normalized,
    /// in-root result.
    #[tokio::test]
    async fn put_rejects_traversal_key_without_creating_dirs_outside_root() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");
        // Escapes only the root's own directory, landing in the root's
        // parent (still inside the writable system temp dir) — not the
        // filesystem root, so the test doesn't depend on `/` permissions.
        let escape_target = svc.root.parent().unwrap().join("evil");

        let err = svc
            .put("f", "../../evil/x", b"data", "text/plain")
            .await
            .expect_err("traversal key must be rejected");
        match err {
            StorageError::InvalidArgument(msg) => assert!(
                msg.contains("path traversal"),
                "expected traversal error, got: {msg}"
            ),
            other => panic!("unexpected error variant: {other:?}"),
        }

        assert!(
            !escape_target.exists(),
            "no directory may be created outside the storage root, found {escape_target:?}"
        );
    }

    /// Regression: `create_folder` used to call `fs::create_dir_all` on the
    /// raw joined path BEFORE `validate_path`, identical to the `put` bug
    /// above. A traversal folder name materialized a directory *outside*
    /// the storage root before the create was ever refused. Validation
    /// must happen first, and `create_dir_all` must only ever run on the
    /// normalized, in-root result.
    #[tokio::test]
    async fn create_folder_rejects_traversal_name_without_creating_dirs_outside_root() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");
        let escape_target = svc.root.parent().unwrap().join("evil");

        let err = svc
            .create_folder("../evil", false)
            .await
            .expect_err("traversal name must be rejected");
        match err {
            StorageError::InvalidArgument(msg) => assert!(
                msg.contains("path traversal"),
                "expected traversal error, got: {msg}"
            ),
            other => panic!("unexpected error variant: {other:?}"),
        }

        assert!(
            !escape_target.exists(),
            "no directory may be created outside the storage root, found {escape_target:?}"
        );
    }

    /// Regression: `delete_folder` used to check `path.exists()` BEFORE
    /// `validate_path`, turning filesystem existence into an oracle for
    /// out-of-root paths: a traversal name was only rejected with the
    /// traversal error if the resolved target happened to exist; if it
    /// didn't exist, the buggy code early-returned `NotFound` instead
    /// (silently confirming *non*-existence of an out-of-root path).
    /// The target below is guaranteed absent (it resolves to a filesystem-
    /// root-level path that nothing creates), so this specifically
    /// exercises that divergence: validation must run first so a traversal
    /// name is always rejected with the traversal error, never treated as
    /// a (non-)existence question.
    #[tokio::test]
    async fn delete_folder_rejects_traversal_name_before_existence_check() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");

        let err = svc
            .delete_folder("../../wafer-local-storage-nonexistent-delete-folder-wr10")
            .await
            .expect_err("traversal name must be rejected");
        match err {
            StorageError::InvalidArgument(msg) => assert!(
                msg.contains("path traversal"),
                "expected traversal error, got: {msg}"
            ),
            other => panic!(
                "unexpected error variant: {other:?}, expected traversal error (got NotFound would mean the pre-validation existence check fired again)"
            ),
        }
    }

    /// Regression: `list` used to check `dir.exists()` BEFORE
    /// `validate_path`, turning filesystem existence into an oracle for
    /// out-of-root paths: a traversal folder that didn't exist silently
    /// returned an empty list (`Ok`) instead of being rejected, while one
    /// that DID exist fell through to validation and got the traversal
    /// error — so existence alone decided whether traversal was even
    /// checked. The target below is guaranteed absent (it resolves to a
    /// filesystem-root-level path that nothing creates), so this
    /// specifically exercises that divergence: validation must run first
    /// so a traversal name is always rejected with the traversal error,
    /// never treated as an empty-list case.
    #[tokio::test]
    async fn list_rejects_traversal_folder_before_existence_check() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");

        let opts = ListOptions {
            prefix: String::new(),
            offset: 0,
            limit: 0,
            cursor: None,
        };
        let err = svc
            .list("../../wafer-local-storage-nonexistent-list-wr10", &opts)
            .await
            .expect_err("traversal folder must be rejected");
        match err {
            StorageError::InvalidArgument(msg) => assert!(
                msg.contains("path traversal"),
                "expected traversal error, got: {msg}"
            ),
            other => panic!(
                "unexpected error variant: {other:?}, expected traversal error (got Ok(empty list) would mean the pre-validation existence check fired again)"
            ),
        }
    }

    /// The real streaming overrides must round-trip: `put_streaming` writes a
    /// multi-chunk `InputStream` to disk chunk-by-chunk, and `get_streaming`
    /// reads it back across its own chunk stream. Both must agree with the
    /// buffered `get` on bytes and size.
    #[tokio::test]
    async fn put_streaming_then_get_streaming_round_trips_chunks() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");
        const EXPECTED: &[u8] = b"hello streamed world";

        // Multi-chunk input — exercises the incremental write path.
        let input = InputStream::from_stream(futures::stream::iter(vec![
            Ok(b"hello ".to_vec()),
            Ok(b"streamed ".to_vec()),
            Ok(b"world".to_vec()),
        ]));
        svc.put_streaming("f", "greeting.txt", input, "text/plain")
            .await
            .expect("put_streaming");

        // Buffered get sees the fully-written object.
        let (buffered, info) = svc.get("f", "greeting.txt").await.expect("get");
        assert_eq!(buffered, EXPECTED);
        assert_eq!(info.size, EXPECTED.len() as i64);

        // Streaming get yields the same bytes across its chunk stream.
        let (stream, sinfo) = svc
            .get_streaming("f", "greeting.txt")
            .await
            .expect("get_streaming");
        assert_eq!(sinfo.size, EXPECTED.len() as i64);
        let body = stream
            .collect_buffered()
            .await
            .expect("stream ends with a Complete terminal")
            .body;
        assert_eq!(body, EXPECTED);

        // Atomicity: the temp file was renamed onto the key, not left behind —
        // the folder holds exactly the object, no `.tmp.` residue.
        let listing = svc.list("f", &ListOptions::default()).await.expect("list");
        let keys: Vec<&str> = listing.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["greeting.txt"],
            "atomic write must leave no temp-file residue"
        );
    }

    /// Overwriting an existing key is atomic: the new bytes fully replace the
    /// old ones (via temp-file + rename), never a mix, and no temp residue is
    /// left behind.
    #[tokio::test]
    async fn put_overwrites_atomically_without_residue() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");

        svc.put("f", "k.bin", b"original-longer-content", "text/plain")
            .await
            .expect("first put");
        svc.put("f", "k.bin", b"new", "text/plain")
            .await
            .expect("overwrite put");

        let (data, _info) = svc.get("f", "k.bin").await.expect("get");
        assert_eq!(data, b"new", "overwrite must fully replace, not merge");

        let listing = svc.list("f", &ListOptions::default()).await.expect("list");
        let keys: Vec<&str> = listing.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, vec!["k.bin"], "no temp-file residue after overwrite");
    }

    /// Cursor mode: an empty cursor returns the first page plus a `next_cursor`;
    /// feeding that token back returns the next objects with no overlap and no
    /// gap; the final page reports `next_cursor: None`. `offset` is ignored in
    /// cursor mode, and `total_count` stays the exact match count throughout.
    #[tokio::test]
    async fn list_by_cursor_pages_forward_without_overlap_or_gap() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");
        // Five objects; written out of order to prove the sort makes paging
        // deterministic regardless of `read_dir` order.
        for k in ["d.txt", "b.txt", "e.txt", "a.txt", "c.txt"] {
            svc.put("f", k, b"x", "text/plain").await.expect("put");
        }

        // Page 1 — bootstrap with an empty cursor; `offset` is set large to
        // prove cursor mode ignores it.
        let p1 = svc
            .list(
                "f",
                &ListOptions {
                    prefix: String::new(),
                    limit: 2,
                    offset: 99,
                    cursor: Some(String::new()),
                },
            )
            .await
            .expect("cursor page 1");
        let p1_keys: Vec<&str> = p1.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(p1_keys, vec!["a.txt", "b.txt"], "sorted first page");
        assert_eq!(p1.total_count, 5, "total_count is the exact match count");
        let c1 = p1.next_cursor.clone().expect("more pages remain");

        // Page 2 — resume from page 1's cursor.
        let p2 = svc
            .list(
                "f",
                &ListOptions {
                    prefix: String::new(),
                    limit: 2,
                    offset: 0,
                    cursor: Some(c1),
                },
            )
            .await
            .expect("cursor page 2");
        let p2_keys: Vec<&str> = p2.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(p2_keys, vec!["c.txt", "d.txt"], "no overlap, no gap");
        let c2 = p2.next_cursor.clone().expect("one more page remains");

        // Page 3 — the final page: one object left, no further cursor.
        let p3 = svc
            .list(
                "f",
                &ListOptions {
                    prefix: String::new(),
                    limit: 2,
                    offset: 0,
                    cursor: Some(c2),
                },
            )
            .await
            .expect("cursor page 3");
        let p3_keys: Vec<&str> = p3.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(p3_keys, vec!["e.txt"]);
        assert_eq!(p3.next_cursor, None, "final page has no continuation token");
    }

    /// Offset pagination is unchanged by the cursor addition: `skip(offset)`
    /// / `take(limit)` over the sorted keys, exact `total_count`, and no
    /// `next_cursor` is ever emitted in offset mode.
    #[tokio::test]
    async fn list_offset_pagination_unchanged_and_emits_no_cursor() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");
        for k in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            svc.put("f", k, b"x", "text/plain").await.expect("put");
        }

        let page = svc
            .list(
                "f",
                &ListOptions {
                    prefix: String::new(),
                    limit: 2,
                    offset: 1,
                    cursor: None,
                },
            )
            .await
            .expect("offset list");
        let keys: Vec<&str> = page.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, vec!["b.txt", "c.txt"], "objects offset..offset+limit");
        assert_eq!(page.total_count, 4);
        assert_eq!(
            page.next_cursor, None,
            "offset mode never emits a cursor (unchanged behavior)"
        );
    }

    /// A malformed cursor (not valid base64) is a client error, not a panic.
    #[tokio::test]
    async fn list_rejects_malformed_cursor() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");
        svc.put("f", "a.txt", b"x", "text/plain")
            .await
            .expect("put");

        let err = svc
            .list(
                "f",
                &ListOptions {
                    prefix: String::new(),
                    limit: 2,
                    offset: 0,
                    cursor: Some("not valid base64!!!".into()),
                },
            )
            .await
            .expect_err("malformed cursor must be rejected");
        match err {
            StorageError::InvalidArgument(msg) => {
                assert!(
                    msg.contains("cursor"),
                    "expected a cursor error, got: {msg}"
                )
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    /// A file past [`DEFAULT_MAX_OBJECT_BYTES`] is refused by both reads with
    /// `TooLarge`, which the storage handler maps to `ResourceExhausted`. The
    /// file is sparse, so the test writes no real data.
    #[tokio::test]
    async fn get_and_get_streaming_refuse_a_file_over_the_cap() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");
        svc.put("f", "huge.bin", b"", "application/octet-stream")
            .await
            .expect("put");
        fs::OpenOptions::new()
            .write(true)
            .open(tmp.join("f").join("huge.bin"))
            .expect("open")
            .set_len(DEFAULT_MAX_OBJECT_BYTES + 1)
            .expect("extend sparse");

        match svc.get("f", "huge.bin").await {
            Err(StorageError::TooLarge(_)) => {}
            other => panic!("get: expected TooLarge, got {:?}", other.map(|(_, i)| i)),
        }
        match svc.get_streaming("f", "huge.bin").await {
            Err(StorageError::TooLarge(_)) => {}
            other => panic!(
                "get_streaming: expected TooLarge, got {:?}",
                other.map(|(_, i)| i)
            ),
        }
        fs::remove_dir_all(&tmp).ok();
    }

    fn body_failure() -> WaferError {
        WaferError::new(ErrorCode::DeadlineExceeded, "request body timed out")
    }

    /// A body that fails after its first chunk is not a shorter object: the
    /// write returns the body's error and nothing appears at the key.
    #[tokio::test]
    async fn put_streaming_that_fails_midway_stores_nothing() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");

        let input = InputStream::from_stream(futures::stream::iter(vec![
            Ok(b"first half".to_vec()),
            Err(body_failure()),
        ]));
        match svc.put_streaming("f", "k", input, "text/plain").await {
            Err(StorageError::Body(e)) => assert_eq!(e, body_failure()),
            other => panic!("expected the body's failure, got {other:?}"),
        }

        assert!(
            matches!(svc.get("f", "k").await, Err(StorageError::NotFound)),
            "a failed body must not be committed at the key"
        );
        let staged: Vec<_> = fs::read_dir(svc.staging_path())
            .expect("read staging")
            .collect();
        assert!(staged.is_empty(), "the staged temp file is removed");
    }

    /// A failed overwrite leaves the previous object in place, whole.
    #[tokio::test]
    async fn put_streaming_that_fails_keeps_the_previous_object() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");
        svc.put("f", "k", b"previous", "text/plain")
            .await
            .expect("put");

        let input = InputStream::from_stream(futures::stream::iter(vec![
            Ok(b"replacement, truncated".to_vec()),
            Err(body_failure()),
        ]));
        svc.put_streaming("f", "k", input, "text/plain")
            .await
            .expect_err("a failed body is an error");

        let (data, _) = svc.get("f", "k").await.expect("get");
        assert_eq!(data, b"previous");
    }

    /// While a write is in flight its temp file is outside every folder:
    /// neither a folder listing, a root listing nor `list_folders` shows it.
    #[tokio::test]
    async fn list_during_an_in_flight_put_streaming_shows_no_temp_file() {
        use futures::channel::oneshot;

        let tmp = tempdir();
        let svc = std::sync::Arc::new(LocalStorageService::new(&tmp).expect("create svc"));

        // Yields one chunk; the next poll (which the service makes only after
        // writing that chunk) reports in and then waits for `release`.
        let (written_tx, written_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let body = futures::stream::unfold(
            (Some(written_tx), Some(release_rx), true),
            |(written, release, first)| async move {
                if first {
                    return Some((Ok(b"chunk".to_vec()), (written, release, false)));
                }
                if let Some(tx) = written {
                    let _ = tx.send(());
                }
                if let Some(rx) = release {
                    let _ = rx.await;
                }
                None
            },
        );
        let writer = {
            let svc = svc.clone();
            tokio::spawn(async move {
                svc.put_streaming("f", "k", InputStream::from_stream(body), "text/plain")
                    .await
            })
        };
        written_rx.await.expect("the first chunk was written");

        let in_folder = svc.list("f", &ListOptions::default()).await.expect("list");
        assert!(
            in_folder.objects.is_empty(),
            "folder listing shows an in-flight write: {:?}",
            in_folder.objects
        );
        let at_root = svc.list("", &ListOptions::default()).await.expect("list");
        assert!(
            at_root.objects.is_empty(),
            "root listing shows an in-flight write: {:?}",
            at_root.objects
        );
        let folders = svc.list_folders().await.expect("list_folders");
        assert!(
            folders.iter().all(|f| f.name != STAGING_DIR),
            "list_folders shows the staging directory"
        );

        release_tx.send(()).expect("release the body");
        writer.await.expect("join").expect("put_streaming");
        let done = svc.list("f", &ListOptions::default()).await.expect("list");
        let keys: Vec<&str> = done.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, vec!["k"]);
    }

    /// A process that stopped mid-write leaves its temp file behind; the next
    /// service over the same root deletes it.
    #[tokio::test]
    async fn new_sweeps_staged_writes_left_by_a_stopped_process() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");
        let leftover = svc.staging_path().join("4242.0.tmp");
        fs::write(&leftover, b"half an upload").expect("write leftover");
        drop(svc);

        let svc = LocalStorageService::new(&tmp).expect("reopen svc");
        assert!(!leftover.exists(), "startup must delete staged leftovers");
        assert!(svc.staging_path().is_dir());
    }

    /// No request may address the staging directory: it is not a folder.
    #[tokio::test]
    async fn staging_directory_is_not_addressable() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");

        let put = svc.put(STAGING_DIR, "x", b"x", "text/plain").await;
        assert!(
            matches!(put, Err(StorageError::InvalidArgument(_))),
            "put into staging: {put:?}"
        );
        let list = svc.list(STAGING_DIR, &ListOptions::default()).await;
        assert!(
            matches!(list, Err(StorageError::InvalidArgument(_))),
            "list of staging: {list:?}"
        );
        let delete = svc.delete_folder(STAGING_DIR).await;
        assert!(
            matches!(delete, Err(StorageError::InvalidArgument(_))),
            "delete of staging: {delete:?}"
        );
    }

    /// Every spelling of the staging directory is refused, not only the
    /// exact one: on a case-insensitive filesystem `.WAFER-STAGING` is the
    /// staging directory, and `delete_folder` must not remove it.
    #[tokio::test]
    async fn staging_directory_is_not_addressable_in_another_case() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");

        for name in [".WAFER-STAGING", ".Wafer-Staging"] {
            let delete = svc.delete_folder(name).await;
            assert!(
                matches!(delete, Err(StorageError::InvalidArgument(_))),
                "delete of {name}: {delete:?}"
            );
            let put = svc.put(name, "x", b"x", "text/plain").await;
            assert!(
                matches!(put, Err(StorageError::InvalidArgument(_))),
                "put into {name}: {put:?}"
            );
        }
        assert!(svc.staging_path().is_dir(), "staging survives");
    }

    /// A spelling the filesystem resolves to the staging directory — what a
    /// case-insensitive filesystem does with a non-ASCII case fold, stood in
    /// for here by a symlink — is refused too, and not listed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_name_that_resolves_to_the_staging_directory_is_refused() {
        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");
        std::os::unix::fs::symlink(svc.staging_path(), svc.root.join("alias")).expect("symlink");
        fs::write(svc.staging_path().join("1.0.tmp"), b"in flight").expect("stage");

        let delete = svc.delete_folder("alias").await;
        assert!(
            matches!(delete, Err(StorageError::InvalidArgument(_))),
            "delete through an alias: {delete:?}"
        );
        let at_root = svc.list("", &ListOptions::default()).await.expect("list");
        assert!(at_root.objects.is_empty(), "{:?}", at_root.objects);
        let folders = svc.list_folders().await.expect("list_folders");
        assert!(
            folders.iter().all(|f| f.name != "alias"),
            "list_folders shows the staging directory under an alias"
        );
    }

    /// A folder on another filesystem (here a symlink onto one) cannot take a
    /// rename from the staging directory; the write fails with an error that
    /// names the cause instead of a bare EXDEV. Needs a writable directory on
    /// a different device from the temp dir (`/dev/shm` on Linux); where
    /// there is none the test has nothing to exercise and says so.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_folder_on_another_filesystem_fails_with_a_clear_error() {
        use std::os::unix::fs::MetadataExt;

        let tmp = tempdir();
        let svc = LocalStorageService::new(&tmp).expect("create svc");
        let root_dev = fs::metadata(&svc.root).expect("root metadata").dev();
        let Some(other) = [PathBuf::from("/dev/shm")]
            .into_iter()
            .find(|p| fs::metadata(p).is_ok_and(|m| m.is_dir() && m.dev() != root_dev))
        else {
            eprintln!("skipped: no writable directory on another filesystem");
            return;
        };
        let elsewhere = other.join(format!(
            "wafer-local-storage-xdev-{}-{}",
            std::process::id(),
            TEMP_WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&elsewhere).expect("create dir on the other filesystem");
        std::os::unix::fs::symlink(&elsewhere, svc.root.join("mounted")).expect("symlink");

        let put = svc.put("mounted", "k", b"x", "text/plain").await;
        fs::remove_dir_all(&elsewhere).ok();
        match put {
            Err(StorageError::Internal(msg)) => assert!(
                msg.contains("different filesystem"),
                "the error must name the cause: {msg}"
            ),
            other => panic!("expected the cross-filesystem error, got {other:?}"),
        }
        let staged: Vec<_> = fs::read_dir(svc.staging_path())
            .expect("read staging")
            .collect();
        assert!(staged.is_empty(), "the staged temp file is removed");
    }

    /// Tests of this process that have taken a tempdir.
    static TEMPDIR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    // Minimal tempdir helper to avoid pulling in a new dev-dep just for this.
    // Unique per test: the counter separates tests running in parallel
    // (their clock readings can be equal), the pid and time separate
    // processes, and `create_dir` fails rather than share a directory that
    // already exists.
    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let pid = std::process::id();
        let seq = TEMPDIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = base.join(format!("wafer-local-storage-test-{pid}-{nonce}-{seq}"));
        fs::create_dir(&dir).expect("create tempdir");
        dir
    }
}
