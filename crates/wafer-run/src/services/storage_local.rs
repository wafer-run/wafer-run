use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};

use super::storage::*;

/// Local filesystem implementation of StorageService.
pub struct LocalStorageService {
    root: PathBuf,
}

impl LocalStorageService {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| {
            StorageError::Internal(format!("create storage root {:?}: {}", root, e))
        })?;
        Ok(Self { root })
    }

    fn folder_path(&self, folder: &str) -> PathBuf {
        self.root.join(folder)
    }

    fn object_path(&self, folder: &str, key: &str) -> PathBuf {
        self.root.join(folder).join(key)
    }

    fn guess_content_type(key: &str) -> String {
        let ext = Path::new(key)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "html" | "htm" => "text/html",
            "css" => "text/css",
            "js" | "mjs" => "application/javascript",
            "json" => "application/json",
            "xml" => "application/xml",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            "ico" => "image/x-icon",
            "pdf" => "application/pdf",
            "zip" => "application/zip",
            "wasm" => "application/wasm",
            "txt" => "text/plain",
            "md" => "text/markdown",
            "csv" => "text/csv",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            "otf" => "font/otf",
            "mp4" => "video/mp4",
            "webm" => "video/webm",
            "mp3" => "audio/mpeg",
            "ogg" => "audio/ogg",
            _ => "application/octet-stream",
        }
        .to_string()
    }
}

impl StorageService for LocalStorageService {
    fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        _content_type: &str,
    ) -> Result<(), StorageError> {
        let path = self.object_path(folder, key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                StorageError::Internal(format!("create dirs for {:?}: {}", path, e))
            })?;
        }
        fs::write(&path, data)
            .map_err(|e| StorageError::Internal(format!("write {:?}: {}", path, e)))
    }

    fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        let path = self.object_path(folder, key);
        if !path.exists() {
            return Err(StorageError::NotFound);
        }

        let data = fs::read(&path)
            .map_err(|e| StorageError::Internal(format!("read {:?}: {}", path, e)))?;

        let metadata = fs::metadata(&path)
            .map_err(|e| StorageError::Internal(format!("metadata {:?}: {}", path, e)))?;

        let last_modified = metadata
            .modified()
            .map(|t| chrono::DateTime::<Utc>::from(t))
            .unwrap_or_else(|_| Utc::now());

        let info = ObjectInfo {
            key: key.to_string(),
            size: data.len() as i64,
            content_type: Self::guess_content_type(key),
            last_modified,
        };

        Ok((data, info))
    }

    fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
        let path = self.object_path(folder, key);
        if !path.exists() {
            return Err(StorageError::NotFound);
        }
        fs::remove_file(&path)
            .map_err(|e| StorageError::Internal(format!("delete {:?}: {}", path, e)))
    }

    fn list(&self, folder: &str, opts: &ListOptions) -> Result<ObjectList, StorageError> {
        let dir = self.folder_path(folder);
        if !dir.exists() {
            return Ok(ObjectList {
                objects: Vec::new(),
                total_count: 0,
            });
        }

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

    fn create_folder(&self, name: &str, _public: bool) -> Result<(), StorageError> {
        let path = self.folder_path(name);
        fs::create_dir_all(&path)
            .map_err(|e| StorageError::Internal(format!("create folder {:?}: {}", path, e)))
    }

    fn delete_folder(&self, name: &str) -> Result<(), StorageError> {
        let path = self.folder_path(name);
        if !path.exists() {
            return Err(StorageError::NotFound);
        }
        fs::remove_dir_all(&path)
            .map_err(|e| StorageError::Internal(format!("delete folder {:?}: {}", path, e)))
    }

    fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        let mut folders = Vec::new();
        let entries = fs::read_dir(&self.root)
            .map_err(|e| StorageError::Internal(format!("read dir {:?}: {}", self.root, e)))?;

        for entry in entries {
            let entry =
                entry.map_err(|e| StorageError::Internal(format!("read entry: {}", e)))?;
            let metadata = entry
                .metadata()
                .map_err(|e| StorageError::Internal(format!("metadata: {}", e)))?;
            if metadata.is_dir() {
                let created_at = metadata
                    .created()
                    .map(|t| chrono::DateTime::<Utc>::from(t))
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
            .map_err(|e| StorageError::Internal(format!("read dir {:?}: {}", dir, e)))?;

        for entry in entries {
            let entry =
                entry.map_err(|e| StorageError::Internal(format!("read entry: {}", e)))?;
            let path = entry.path();
            let metadata = entry
                .metadata()
                .map_err(|e| StorageError::Internal(format!("metadata: {}", e)))?;

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
                    .map(|t| chrono::DateTime::<Utc>::from(t))
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
