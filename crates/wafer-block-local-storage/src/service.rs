use std::{
    fs,
    path::{Component, Path, PathBuf},
};
use wafer_block_macro::wafer_async_trait;

use chrono::Utc;
use wafer_core::interfaces::storage::service::*;

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

/// Local filesystem implementation of StorageService.
pub struct LocalStorageService {
    root: PathBuf,
}

impl LocalStorageService {
    /// Construct a service rooted at `root`, creating the directory tree if it
    /// does not yet exist. All subsequent reads/writes are confined to this
    /// root via [`Self::validate_path`].
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root = root.into();
        fs::create_dir_all(&root)
            .map_err(|e| StorageError::Internal(format!("create storage root {root:?}: {e}")))?;
        Ok(Self { root })
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
            StorageError::Internal("path traversal: resolved path escapes storage root".to_string())
        })?;

        if !normalized.starts_with(&canon_root) {
            return Err(StorageError::Internal(
                "path traversal: resolved path escapes storage root".to_string(),
            ));
        }
        Ok(normalized)
    }

    fn guess_content_type(key: &str) -> String {
        wafer_core::mime::mime_for_ext(Path::new(key)).to_string()
    }
}

#[wafer_async_trait]
impl StorageService for LocalStorageService {
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        _content_type: &str,
    ) -> Result<(), StorageError> {
        let path = self.object_path(folder, key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| StorageError::Internal(format!("create dirs for {path:?}: {e}")))?;
        }
        // Validate after parent dirs are created so canonicalize can resolve
        let path = self.validate_path(&path)?;
        fs::write(&path, data).map_err(|e| StorageError::Internal(format!("write {path:?}: {e}")))
    }

    async fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        let path = self.object_path(folder, key);
        if !path.exists() {
            return Err(StorageError::NotFound);
        }
        let path = self.validate_path(&path)?;

        let metadata = fs::metadata(&path)
            .map_err(|e| StorageError::Internal(format!("metadata {path:?}: {e}")))?;

        // Limit file reads to 100 MB to prevent OOM on huge files
        const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;
        if metadata.len() > MAX_FILE_SIZE {
            return Err(StorageError::Internal(format!(
                "file {:?} is {} bytes, exceeds limit of {} bytes",
                path,
                metadata.len(),
                MAX_FILE_SIZE
            )));
        }

        let data =
            fs::read(&path).map_err(|e| StorageError::Internal(format!("read {path:?}: {e}")))?;

        let last_modified = metadata
            .modified()
            .map(chrono::DateTime::<Utc>::from)
            .unwrap_or_else(|_| Utc::now());

        let info = ObjectInfo {
            key: key.to_string(),
            size: data.len() as i64,
            content_type: Self::guess_content_type(key),
            last_modified,
        };

        Ok((data, info))
    }

    async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
        let path = self.object_path(folder, key);
        if !path.exists() {
            return Err(StorageError::NotFound);
        }
        let path = self.validate_path(&path)?;
        fs::remove_file(&path).map_err(|e| StorageError::Internal(format!("delete {path:?}: {e}")))
    }

    async fn list(&self, folder: &str, opts: &ListOptions) -> Result<ObjectList, StorageError> {
        let dir = self.folder_path(folder);
        if !dir.exists() {
            return Ok(ObjectList {
                objects: Vec::new(),
                total_count: 0,
            });
        }
        self.validate_path(&dir)?;

        let mut objects = Vec::new();
        Self::list_recursive(&dir, &dir, &opts.prefix, &mut objects)?;

        let total_count = objects.len() as i64;

        // Apply pagination
        let offset = opts.offset as usize;
        let limit = if opts.limit > 0 {
            opts.limit as usize
        } else {
            objects.len()
        };

        let objects: Vec<ObjectInfo> = objects.into_iter().skip(offset).take(limit).collect();

        Ok(ObjectList {
            objects,
            total_count,
        })
    }

    async fn create_folder(&self, name: &str, _public: bool) -> Result<(), StorageError> {
        let path = self.folder_path(name);
        // Create the directory first so validate_path can canonicalize
        fs::create_dir_all(&path)
            .map_err(|e| StorageError::Internal(format!("create folder {path:?}: {e}")))?;
        self.validate_path(&path)?;
        Ok(())
    }

    async fn delete_folder(&self, name: &str) -> Result<(), StorageError> {
        let path = self.folder_path(name);
        if !path.exists() {
            return Err(StorageError::NotFound);
        }
        let path = self.validate_path(&path)?;
        fs::remove_dir_all(&path)
            .map_err(|e| StorageError::Internal(format!("delete folder {path:?}: {e}")))
    }

    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        let mut folders = Vec::new();
        let entries = fs::read_dir(&self.root)
            .map_err(|e| StorageError::Internal(format!("read dir {:?}: {}", self.root, e)))?;

        for entry in entries {
            let entry = entry.map_err(|e| StorageError::Internal(format!("read entry: {e}")))?;
            let metadata = entry
                .metadata()
                .map_err(|e| StorageError::Internal(format!("metadata: {e}")))?;
            if metadata.is_dir() {
                let created_at = metadata
                    .created()
                    .map(chrono::DateTime::<Utc>::from)
                    .unwrap_or_else(|_| Utc::now());
                folders.push(FolderInfo {
                    name: entry.file_name().to_string_lossy().to_string(),
                    public: false,
                    created_at,
                });
            }
        }

        Ok(folders)
    }
}

impl LocalStorageService {
    fn list_recursive(
        base: &Path,
        dir: &Path,
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

            if metadata.is_dir() {
                Self::list_recursive(base, &path, prefix, objects)?;
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
                    .map(chrono::DateTime::<Utc>::from)
                    .unwrap_or_else(|_| Utc::now());

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
            StorageError::Internal(msg) => assert!(
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

    // Minimal tempdir helper to avoid pulling in a new dev-dep just for this.
    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let dir = base.join(format!("wafer-local-storage-test-{pid}-{nonce}"));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }
}
