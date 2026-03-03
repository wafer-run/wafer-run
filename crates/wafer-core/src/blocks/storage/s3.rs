use aws_sdk_s3::Client;
use chrono::{DateTime, Utc};
use tokio::runtime::Handle;

use super::service::*;

/// S3 implementation of StorageService.
///
/// Supports AWS S3, MinIO, Tigris, Cloudflare R2, and any S3-compatible
/// object store via custom endpoint configuration.
///
/// Objects are stored under `{prefix}/{folder}/{key}` for tenant isolation.
/// Folders are represented by zero-length objects with a trailing `/` key.
pub struct S3StorageService {
    client: Client,
    bucket: String,
    prefix: String,
}

impl S3StorageService {
    /// Create with default AWS config (env vars / IAM role).
    pub async fn new(bucket: &str, prefix: &str) -> Result<Self, StorageError> {
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let client = Client::new(&config);
        Ok(Self {
            client,
            bucket: bucket.to_string(),
            prefix: prefix.to_string(),
        })
    }

    /// Create with a custom endpoint for MinIO/Tigris/R2 compatibility.
    pub async fn with_endpoint(
        bucket: &str,
        prefix: &str,
        endpoint: &str,
        region: &str,
    ) -> Result<Self, StorageError> {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .endpoint_url(endpoint)
            .region(aws_config::Region::new(region.to_string()))
            .load()
            .await;
        let s3_config = aws_sdk_s3::config::Builder::from(&config)
            .force_path_style(true) // needed for MinIO
            .build();
        let client = Client::from_conf(s3_config);
        Ok(Self {
            client,
            bucket: bucket.to_string(),
            prefix: prefix.to_string(),
        })
    }

    /// Build the full S3 key for an object: `{prefix}/{folder}/{key}`.
    fn s3_key(&self, folder: &str, key: &str) -> String {
        if self.prefix.is_empty() {
            format!("{}/{}", folder, key)
        } else {
            format!("{}/{}/{}", self.prefix, folder, key)
        }
    }

    /// Build the S3 prefix for listing objects within a folder.
    fn folder_prefix(&self, folder: &str) -> String {
        if self.prefix.is_empty() {
            format!("{}/", folder)
        } else {
            format!("{}/{}/", self.prefix, folder)
        }
    }

    /// Build the S3 prefix for listing top-level folders.
    fn root_prefix(&self) -> String {
        if self.prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", self.prefix)
        }
    }

    /// Convert an aws_smithy_types::DateTime to chrono::DateTime<Utc>.
    fn to_chrono_datetime(dt: &aws_sdk_s3::primitives::DateTime) -> DateTime<Utc> {
        DateTime::from_timestamp(dt.secs(), dt.subsec_nanos())
            .unwrap_or_else(Utc::now)
    }

    /// Run an async block synchronously via `block_in_place` + `block_on`.
    fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        let handle = Handle::current();
        tokio::task::block_in_place(|| handle.block_on(f))
    }
}

impl StorageService for S3StorageService {
    fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), StorageError> {
        let s3_key = self.s3_key(folder, key);
        let body = aws_sdk_s3::primitives::ByteStream::from(data.to_vec());

        self.block_on(async {
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(&s3_key)
                .body(body)
                .content_type(content_type)
                .send()
                .await
                .map_err(|e| StorageError::Internal(format!("S3 PutObject {}: {}", s3_key, e)))?;
            Ok(())
        })
    }

    fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        let s3_key = self.s3_key(folder, key);

        self.block_on(async {
            let resp = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(&s3_key)
                .send()
                .await
                .map_err(|e| {
                    // Check for NoSuchKey / 404
                    let svc_err = e.into_service_error();
                    if svc_err.is_no_such_key() {
                        StorageError::NotFound
                    } else {
                        StorageError::Internal(format!("S3 GetObject {}: {}", s3_key, svc_err))
                    }
                })?;

            let content_type = resp
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_string();

            let content_length = resp.content_length().unwrap_or(0);

            let last_modified = resp
                .last_modified()
                .map(Self::to_chrono_datetime)
                .unwrap_or_else(Utc::now);

            let body = resp
                .body
                .collect()
                .await
                .map_err(|e| {
                    StorageError::Internal(format!("S3 read body {}: {}", s3_key, e))
                })?
                .into_bytes()
                .to_vec();

            let info = ObjectInfo {
                key: key.to_string(),
                size: content_length,
                content_type,
                last_modified,
            };

            Ok((body, info))
        })
    }

    fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
        let s3_key = self.s3_key(folder, key);

        self.block_on(async {
            self.client
                .delete_object()
                .bucket(&self.bucket)
                .key(&s3_key)
                .send()
                .await
                .map_err(|e| StorageError::Internal(format!("S3 DeleteObject {}: {}", s3_key, e)))?;
            Ok(())
        })
    }

    fn list(&self, folder: &str, opts: &ListOptions) -> Result<ObjectList, StorageError> {
        let prefix = self.folder_prefix(folder);
        let search_prefix = if opts.prefix.is_empty() {
            prefix.clone()
        } else {
            format!("{}{}", prefix, opts.prefix)
        };

        self.block_on(async {
            let mut all_objects = Vec::new();
            let mut continuation_token: Option<String> = None;

            loop {
                let mut req = self
                    .client
                    .list_objects_v2()
                    .bucket(&self.bucket)
                    .prefix(&search_prefix);

                if let Some(ref token) = continuation_token {
                    req = req.continuation_token(token);
                }

                let resp = req.send().await.map_err(|e| {
                    StorageError::Internal(format!("S3 ListObjectsV2 {}: {}", search_prefix, e))
                })?;

                for obj in resp.contents() {
                    let full_key = obj.key().unwrap_or_default();

                    // Strip the folder prefix to get the relative key
                    let relative_key = full_key
                        .strip_prefix(&prefix)
                        .unwrap_or(full_key)
                        .to_string();

                    // Skip the folder marker itself (empty key after prefix strip)
                    if relative_key.is_empty() {
                        continue;
                    }

                    let last_modified = obj
                        .last_modified()
                        .map(Self::to_chrono_datetime)
                        .unwrap_or_else(Utc::now);

                    all_objects.push(ObjectInfo {
                        key: relative_key,
                        size: obj.size().unwrap_or(0),
                        content_type: String::new(), // S3 ListObjects doesn't return content-type
                        last_modified,
                    });
                }

                if resp.is_truncated() == Some(true) {
                    continuation_token = resp.next_continuation_token().map(|s| s.to_string());
                } else {
                    break;
                }
            }

            let total_count = all_objects.len() as i64;

            // Apply pagination (offset / limit)
            let offset = opts.offset as usize;
            let limit = if opts.limit > 0 {
                opts.limit as usize
            } else {
                all_objects.len()
            };

            let objects: Vec<ObjectInfo> =
                all_objects.into_iter().skip(offset).take(limit).collect();

            Ok(ObjectList {
                objects,
                total_count,
            })
        })
    }

    fn create_folder(&self, name: &str, _public: bool) -> Result<(), StorageError> {
        // Create a zero-length marker object with a trailing `/`.
        let marker_key = self.folder_prefix(name);

        self.block_on(async {
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(&marker_key)
                .body(aws_sdk_s3::primitives::ByteStream::from(Vec::new()))
                .send()
                .await
                .map_err(|e| {
                    StorageError::Internal(format!(
                        "S3 create folder marker {}: {}",
                        marker_key, e
                    ))
                })?;
            Ok(())
        })
    }

    fn delete_folder(&self, name: &str) -> Result<(), StorageError> {
        let prefix = self.folder_prefix(name);

        self.block_on(async {
            // List all objects under this folder prefix and delete them in batches.
            let mut continuation_token: Option<String> = None;

            loop {
                let mut req = self
                    .client
                    .list_objects_v2()
                    .bucket(&self.bucket)
                    .prefix(&prefix);

                if let Some(ref token) = continuation_token {
                    req = req.continuation_token(token);
                }

                let resp = req.send().await.map_err(|e| {
                    StorageError::Internal(format!("S3 list for delete {}: {}", prefix, e))
                })?;

                let contents = resp.contents();

                if !contents.is_empty() {
                    // Build batch delete request
                    let objects_to_delete: Vec<aws_sdk_s3::types::ObjectIdentifier> = contents
                        .iter()
                        .filter_map(|obj| {
                            obj.key().map(|k| {
                                aws_sdk_s3::types::ObjectIdentifier::builder()
                                    .key(k)
                                    .build()
                                    .expect("key is set")
                            })
                        })
                        .collect();

                    if !objects_to_delete.is_empty() {
                        let delete = aws_sdk_s3::types::Delete::builder()
                            .set_objects(Some(objects_to_delete))
                            .build()
                            .map_err(|e| {
                                StorageError::Internal(format!("build Delete request: {}", e))
                            })?;

                        self.client
                            .delete_objects()
                            .bucket(&self.bucket)
                            .delete(delete)
                            .send()
                            .await
                            .map_err(|e| {
                                StorageError::Internal(format!(
                                    "S3 DeleteObjects {}: {}",
                                    prefix, e
                                ))
                            })?;
                    }
                }

                if resp.is_truncated() == Some(true) {
                    continuation_token = resp.next_continuation_token().map(|s| s.to_string());
                } else {
                    break;
                }
            }

            Ok(())
        })
    }

    fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        let prefix = self.root_prefix();

        self.block_on(async {
            let resp = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                .delimiter("/")
                .send()
                .await
                .map_err(|e| {
                    StorageError::Internal(format!("S3 list folders: {}", e))
                })?;

            let mut folders = Vec::new();

            for cp in resp.common_prefixes() {
                if let Some(pfx) = cp.prefix() {
                    // Strip the root prefix and trailing `/` to get the folder name
                    let name = pfx
                        .strip_prefix(&prefix)
                        .unwrap_or(pfx)
                        .trim_end_matches('/');

                    if name.is_empty() {
                        continue;
                    }

                    folders.push(FolderInfo {
                        name: name.to_string(),
                        public: false,       // S3 doesn't track this natively
                        created_at: Utc::now(), // S3 doesn't expose folder creation time
                    });
                }
            }

            Ok(folders)
        })
    }
}
