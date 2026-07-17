//! S3-compatible [`StorageService`] implementation backing the `wafer-run/s3` block.
//!
//! Works against AWS S3 and any S3-compatible endpoint (MinIO, Tigris, Cloudflare R2)
//! via [`S3StorageService::with_endpoint`]. Custom endpoints force path-style addressing
//! for MinIO compatibility. Objects are namespaced as `{prefix}/{folder}/{key}` and
//! folders are materialized as zero-length marker objects ending in `/`.

use aws_sdk_s3::Client;
use chrono::{DateTime, Utc};
use wafer_block::{common::ErrorCode, OutputStream, WaferError};
use wafer_block_macro::wafer_async_trait;
use wafer_core::interfaces::storage::service::*;

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
            format!("{folder}/{key}")
        } else {
            format!("{}/{}/{}", self.prefix, folder, key)
        }
    }

    /// Build the S3 prefix for listing objects within a folder.
    fn folder_prefix(&self, folder: &str) -> String {
        if self.prefix.is_empty() {
            format!("{folder}/")
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
        DateTime::from_timestamp(dt.secs(), dt.subsec_nanos()).unwrap_or_else(Utc::now)
    }
}

/// The S3 `ListObjectsV2` per-page key maximum (also the `DeleteObjects`
/// per-request key limit).
const MAX_KEYS_PER_PAGE: usize = 1000;

// Streaming policy: `get_streaming` streams the `GetObject` response body
// (`ByteStream`) without buffering it whole. `put_streaming` deliberately
// keeps the buffered default — S3 `PutObject` requires a known
// `Content-Length`, which an unbounded `InputStream` cannot supply without
// first collecting it; a true streaming upload needs a multipart flow
// (initiate / upload-part / complete) and is deferred to a follow-up.
#[wafer_async_trait]
impl StorageService for S3StorageService {
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), StorageError> {
        let s3_key = self.s3_key(folder, key);
        let body = aws_sdk_s3::primitives::ByteStream::from(data.to_vec());

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&s3_key)
            .body(body)
            .content_type(content_type)
            .send()
            .await
            .map_err(|e| StorageError::Internal(format!("S3 PutObject {s3_key}: {e}")))?;
        Ok(())
    }

    async fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        let s3_key = self.s3_key(folder, key);

        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&s3_key)
            .send()
            .await
            .map_err(|e| {
                let svc_err = e.into_service_error();
                if svc_err.is_no_such_key() {
                    StorageError::NotFound
                } else {
                    StorageError::Internal(format!("S3 GetObject {s3_key}: {svc_err}"))
                }
            })?;

        let content_type = resp
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

        let content_length = resp.content_length().unwrap_or(0);

        let last_modified = resp
            .last_modified()
            .map_or_else(Utc::now, Self::to_chrono_datetime);

        let body = resp
            .body
            .collect()
            .await
            .map_err(|e| StorageError::Internal(format!("S3 read body {s3_key}: {e}")))?
            .into_bytes()
            .to_vec();

        let info = ObjectInfo {
            key: key.to_string(),
            size: content_length,
            content_type,
            last_modified,
        };

        Ok((body, info))
    }

    /// Streams the object body straight from the `GetObject` response
    /// (`ByteStream`) through an [`OutputStream`] producer, so a large object
    /// is never buffered whole in memory (the default `get` collects the
    /// entire body first). `ObjectInfo` is resolved eagerly from the response
    /// head. A body-read failure is surfaced as an `Error` terminal.
    ///
    /// Enforces the same 100 MiB object cap as local-storage `get` so a huge
    /// object cannot stream unbounded into an isolate: an advertised
    /// `Content-Length` over the cap is rejected up front, and the running
    /// total is checked per chunk (defending against an absent or underreported
    /// length), surfacing an `Error` terminal on overflow.
    async fn get_streaming(
        &self,
        folder: &str,
        key: &str,
    ) -> Result<(OutputStream, ObjectInfo), StorageError> {
        // Parity with local-storage's `get`/`get_streaming` 100 MiB cap.
        const MAX_STREAM_BYTES: u64 = 100 * 1024 * 1024;

        let s3_key = self.s3_key(folder, key);

        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&s3_key)
            .send()
            .await
            .map_err(|e| {
                let svc_err = e.into_service_error();
                if svc_err.is_no_such_key() {
                    StorageError::NotFound
                } else {
                    StorageError::Internal(format!("S3 GetObject {s3_key}: {svc_err}"))
                }
            })?;

        let content_type = resp
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let content_length = resp.content_length().unwrap_or(0);
        let last_modified = resp
            .last_modified()
            .map_or_else(Utc::now, Self::to_chrono_datetime);

        // Reject up front when the advertised length already exceeds the cap.
        // A negative/unknown length skips this guard; the running total below
        // still bounds it.
        if let Ok(advertised) = u64::try_from(content_length) {
            if advertised > MAX_STREAM_BYTES {
                return Err(StorageError::Internal(format!(
                    "S3 object {s3_key} is {content_length} bytes, exceeds streaming limit of {MAX_STREAM_BYTES} bytes"
                )));
            }
        }

        let info = ObjectInfo {
            key: key.to_string(),
            size: content_length,
            content_type,
            last_modified,
        };

        let mut body = resp.body;
        let stream = OutputStream::from_producer(move |sink, cancel| async move {
            let mut received: u64 = 0;
            loop {
                let next = tokio::select! {
                    biased;
                    // Consumer dropped the stream mid-read — abort promptly
                    // rather than blocking on the upstream S3 read.
                    () = cancel.cancelled() => return,
                    next = body.next() => next,
                };
                match next {
                    Some(Ok(chunk)) => {
                        received = received.saturating_add(chunk.len() as u64);
                        if received > MAX_STREAM_BYTES {
                            let _ = sink
                                .error(WaferError::new(
                                    ErrorCode::Internal,
                                    format!(
                                        "S3 object {s3_key} exceeds streaming limit of {MAX_STREAM_BYTES} bytes"
                                    ),
                                ))
                                .await;
                            return;
                        }
                        if sink.send_chunk(chunk.to_vec()).await.is_err() {
                            // Consumer dropped the stream — stop reading.
                            return;
                        }
                    }
                    Some(Err(e)) => {
                        let _ = sink
                            .error(WaferError::new(
                                ErrorCode::Internal,
                                format!("S3 read body {s3_key}: {e}"),
                            ))
                            .await;
                        return;
                    }
                    None => break,
                }
            }
            let _ = sink.complete(vec![]).await;
        });

        Ok((stream, info))
    }

    async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
        let s3_key = self.s3_key(folder, key);

        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(&s3_key)
            .send()
            .await
            .map_err(|e| StorageError::Internal(format!("S3 DeleteObject {s3_key}: {e}")))?;
        Ok(())
    }

    /// List objects with pagination pushed down to S3 (PERF-04).
    ///
    /// Pages are walked via continuation tokens and the scan stops as soon
    /// as `offset + limit` objects (plus one peek object) have been seen —
    /// the old implementation buffered *every* page before slicing the
    /// window in memory. Each request also caps `MaxKeys` to what the scan
    /// still needs, so the final page is not a full 1000-key fetch.
    ///
    /// `total_count` is exact when the scan reached the end of the listing.
    /// When the scan stopped early it is a lower bound of
    /// `offset + limit + 1` — strictly greater than `offset + limit`, so
    /// "more pages exist" checks (`total_count > offset + limit`) remain
    /// correct. S3 has no way to report an exact total without walking the
    /// whole keyspace.
    async fn list(&self, folder: &str, opts: &ListOptions) -> Result<ObjectList, StorageError> {
        let prefix = self.folder_prefix(folder);
        let search_prefix = if opts.prefix.is_empty() {
            prefix.clone()
        } else {
            format!("{}{}", prefix, opts.prefix)
        };

        let offset = usize::try_from(opts.offset).unwrap_or(0);
        let limit = usize::try_from(opts.limit).ok().filter(|l| *l > 0);
        // Stop scanning once the window plus one peek object has been seen.
        // The peek keeps `total_count` an honest more-pages-exist signal
        // without walking the rest of the keyspace. `limit == 0` means
        // "no limit": scan everything, `total_count` is exact.
        let scan_cap = limit.map(|l| offset.saturating_add(l).saturating_add(1));

        let mut continuation_token: Option<String> = None;
        let mut seen: usize = 0; // objects observed, folder marker excluded
        let mut objects: Vec<ObjectInfo> = Vec::new();

        'pages: loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&search_prefix);
            if let Some(cap) = scan_cap {
                // Ask S3 for no more keys than the scan still needs. The +1
                // leaves room for the folder-marker key (skipped below) so
                // the common case finishes without an extra round trip.
                let remaining = cap - seen + 1;
                req = req.max_keys(remaining.min(MAX_KEYS_PER_PAGE) as i32);
            }
            if let Some(token) = &continuation_token {
                req = req.continuation_token(token);
            }
            let page = req.send().await.map_err(|e| {
                StorageError::Internal(format!("S3 ListObjectsV2 {search_prefix}: {e}"))
            })?;

            for obj in page.contents() {
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

                seen += 1;
                if seen > offset && limit.is_none_or(|l| objects.len() < l) {
                    let last_modified = obj
                        .last_modified()
                        .map_or_else(Utc::now, Self::to_chrono_datetime);

                    objects.push(ObjectInfo {
                        key: relative_key,
                        size: obj.size().unwrap_or(0),
                        content_type: String::new(), // S3 ListObjects doesn't return content-type
                        last_modified,
                    });
                }
                if scan_cap.is_some_and(|cap| seen >= cap) {
                    break 'pages;
                }
            }

            match page.next_continuation_token() {
                Some(token) => continuation_token = Some(token.to_string()),
                None => break,
            }
        }

        Ok(ObjectList {
            objects,
            total_count: seen as i64,
        })
    }

    async fn create_folder(&self, name: &str, _public: bool) -> Result<(), StorageError> {
        // Create a zero-length marker object with a trailing `/`.
        let marker_key = self.folder_prefix(name);

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&marker_key)
            .body(aws_sdk_s3::primitives::ByteStream::from(Vec::new()))
            .send()
            .await
            .map_err(|e| {
                StorageError::Internal(format!("S3 create folder marker {marker_key}: {e}"))
            })?;
        Ok(())
    }

    async fn delete_folder(&self, name: &str) -> Result<(), StorageError> {
        let prefix = self.folder_prefix(name);

        // Stream ListObjectsV2 pages and delete each page's keys in one
        // DeleteObjects batch (a page holds at most 1000 keys — the
        // DeleteObjects request limit) instead of buffering the whole
        // listing first. Deleted keys are always behind the continuation
        // cursor, so paging stays consistent while deleting.
        let mut stream = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(&prefix)
            .into_paginator()
            .send();

        while let Some(page) = stream.next().await {
            let page = page
                .map_err(|e| StorageError::Internal(format!("S3 list for delete {prefix}: {e}")))?;
            // Build batch delete request
            let objects_to_delete: Vec<aws_sdk_s3::types::ObjectIdentifier> = page
                .contents()
                .iter()
                .filter_map(|obj| {
                    let k = obj.key()?;
                    aws_sdk_s3::types::ObjectIdentifier::builder()
                        .key(k)
                        .build()
                        .map_err(|e| {
                            tracing::warn!(key = %k, err = %e, "skip object in batch delete");
                        })
                        .ok()
                })
                .collect();

            if !objects_to_delete.is_empty() {
                let delete = aws_sdk_s3::types::Delete::builder()
                    .set_objects(Some(objects_to_delete))
                    .build()
                    .map_err(|e| StorageError::Internal(format!("build Delete request: {e}")))?;

                self.client
                    .delete_objects()
                    .bucket(&self.bucket)
                    .delete(delete)
                    .send()
                    .await
                    .map_err(|e| {
                        StorageError::Internal(format!("S3 DeleteObjects {prefix}: {e}"))
                    })?;
            }
        }

        Ok(())
    }

    /// List top-level folders, paginating until the delimiter listing is
    /// exhausted. The previous implementation issued a single
    /// `ListObjectsV2` request, silently truncating buckets whose
    /// first page didn't hold every common prefix (PERF-04).
    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        let prefix = self.root_prefix();

        let mut stream = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(&prefix)
            .delimiter("/")
            .into_paginator()
            .send();

        let mut folders = Vec::new();

        while let Some(page) = stream.next().await {
            let page = page.map_err(|e| StorageError::Internal(format!("S3 list folders: {e}")))?;
            for cp in page.common_prefixes() {
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
                        public: false,          // S3 doesn't track this natively
                        created_at: Utc::now(), // S3 doesn't expose folder creation time
                    });
                }
            }
        }

        Ok(folders)
    }
}

#[cfg(test)]
mod tests {
    use aws_sdk_s3::{
        operation::{delete_objects::DeleteObjectsOutput, list_objects_v2::ListObjectsV2Output},
        types::{CommonPrefix, Object},
    };
    use aws_smithy_mocks::{mock, mock_client, RuleMode};

    use super::*;

    /// Service under test wired to a mocked S3 client — no HTTP involved.
    fn service(client: Client) -> S3StorageService {
        S3StorageService {
            client,
            bucket: "bucket".to_string(),
            prefix: String::new(),
        }
    }

    fn obj(key: &str, size: i64) -> Object {
        Object::builder().key(key).size(size).build()
    }

    /// PERF-04: `list` must stop paging as soon as `offset + limit` (plus
    /// the one-object peek) is satisfied, cap `MaxKeys` to what the scan
    /// still needs, and thread continuation tokens between requests. A
    /// third page exists but must never be fetched.
    #[tokio::test]
    async fn list_stops_fetching_once_window_is_satisfied() {
        // offset=1, limit=2 → scan cap 4 (window + 1 peek).
        // First request: no token, MaxKeys = cap - seen + 1 = 5.
        let page1 = mock!(aws_sdk_s3::Client::list_objects_v2)
            .match_requests(|req| {
                req.continuation_token().is_none()
                    && req.max_keys() == Some(5)
                    && req.prefix() == Some("folder/")
            })
            .then_output(|| {
                ListObjectsV2Output::builder()
                    .contents(obj("folder/a", 1))
                    .contents(obj("folder/b", 2))
                    .is_truncated(true)
                    .next_continuation_token("tok1")
                    .build()
            });
        // Second request: token from page 1, MaxKeys = 4 - 2 + 1 = 3.
        let page2 = mock!(aws_sdk_s3::Client::list_objects_v2)
            .match_requests(|req| {
                req.continuation_token() == Some("tok1") && req.max_keys() == Some(3)
            })
            .then_output(|| {
                ListObjectsV2Output::builder()
                    .contents(obj("folder/c", 3))
                    .contents(obj("folder/d", 4))
                    .contents(obj("folder/e", 5))
                    .is_truncated(true)
                    .next_continuation_token("tok2")
                    .build()
            });
        // Third page: exists, but the scan must stop before requesting it.
        let page3 = mock!(aws_sdk_s3::Client::list_objects_v2)
            .match_requests(|req| req.continuation_token() == Some("tok2"))
            .then_output(|| ListObjectsV2Output::builder().build());

        let client = mock_client!(aws_sdk_s3, RuleMode::MatchAny, [&page1, &page2, &page3]);
        let svc = service(client);

        let list = svc
            .list(
                "folder",
                &ListOptions {
                    prefix: String::new(),
                    limit: 2,
                    offset: 1,
                },
            )
            .await
            .expect("list succeeds");

        let keys: Vec<&str> = list.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["b", "c"],
            "window is objects offset..offset+limit"
        );
        assert_eq!(
            list.total_count, 4,
            "early-stopped scan reports the lower bound offset + limit + 1"
        );
        assert_eq!(page1.num_calls(), 1);
        assert_eq!(page2.num_calls(), 1);
        assert_eq!(
            page3.num_calls(),
            0,
            "pages past the satisfied window must not be fetched"
        );
    }

    /// `limit == 0` means no limit: the scan walks every page (no MaxKeys
    /// pushdown), skips the folder-marker key, and `total_count` is exact.
    #[tokio::test]
    async fn list_without_limit_walks_all_pages_and_reports_exact_total() {
        let page1 = mock!(aws_sdk_s3::Client::list_objects_v2)
            .match_requests(|req| req.continuation_token().is_none() && req.max_keys().is_none())
            .then_output(|| {
                ListObjectsV2Output::builder()
                    .contents(obj("folder/", 0)) // folder marker — skipped
                    .contents(obj("folder/a", 1))
                    .contents(obj("folder/b", 2))
                    .is_truncated(true)
                    .next_continuation_token("tok1")
                    .build()
            });
        let page2 = mock!(aws_sdk_s3::Client::list_objects_v2)
            .match_requests(|req| req.continuation_token() == Some("tok1"))
            .then_output(|| {
                ListObjectsV2Output::builder()
                    .contents(obj("folder/c", 3))
                    .build()
            });

        let client = mock_client!(aws_sdk_s3, RuleMode::MatchAny, [&page1, &page2]);
        let svc = service(client);

        let list = svc
            .list("folder", &ListOptions::default())
            .await
            .expect("list succeeds");

        let keys: Vec<&str> = list.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, vec!["a", "b", "c"]);
        assert_eq!(
            list.total_count, 3,
            "exhausted scan reports the exact total"
        );
        assert_eq!(
            page2.num_calls(),
            1,
            "all pages fetched when no limit is set"
        );
    }

    /// Offset beyond the keyspace: empty window, exact total (the scan
    /// exhausted the listing before reaching the cap).
    #[tokio::test]
    async fn list_offset_beyond_total_returns_empty_window_and_exact_total() {
        let page1 = mock!(aws_sdk_s3::Client::list_objects_v2)
            .match_requests(|req| req.continuation_token().is_none())
            .then_output(|| {
                ListObjectsV2Output::builder()
                    .contents(obj("folder/a", 1))
                    .contents(obj("folder/b", 2))
                    .contents(obj("folder/c", 3))
                    .build()
            });

        let client = mock_client!(aws_sdk_s3, RuleMode::MatchAny, [&page1]);
        let svc = service(client);

        let list = svc
            .list(
                "folder",
                &ListOptions {
                    prefix: String::new(),
                    limit: 5,
                    offset: 10,
                },
            )
            .await
            .expect("list succeeds");

        assert!(list.objects.is_empty());
        assert_eq!(list.total_count, 3);
    }

    /// PERF-04: `list_folders` must accumulate common prefixes across
    /// continuation tokens instead of silently truncating at one page.
    #[tokio::test]
    async fn list_folders_accumulates_across_pages() {
        let page1 = mock!(aws_sdk_s3::Client::list_objects_v2)
            .match_requests(|req| {
                req.continuation_token().is_none() && req.delimiter() == Some("/")
            })
            .then_output(|| {
                ListObjectsV2Output::builder()
                    .common_prefixes(CommonPrefix::builder().prefix("alpha/").build())
                    .common_prefixes(CommonPrefix::builder().prefix("beta/").build())
                    .is_truncated(true)
                    .next_continuation_token("tok1")
                    .build()
            });
        let page2 = mock!(aws_sdk_s3::Client::list_objects_v2)
            .match_requests(|req| req.continuation_token() == Some("tok1"))
            .then_output(|| {
                ListObjectsV2Output::builder()
                    .common_prefixes(CommonPrefix::builder().prefix("gamma/").build())
                    .build()
            });

        let client = mock_client!(aws_sdk_s3, RuleMode::MatchAny, [&page1, &page2]);
        let svc = service(client);

        let folders = svc.list_folders().await.expect("list_folders succeeds");
        let names: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["alpha", "beta", "gamma"],
            "folders from every page must be accumulated"
        );
        assert_eq!(page2.num_calls(), 1);
    }

    /// `get_streaming` streams the `GetObject` `ByteStream` body through the
    /// returned `OutputStream` and resolves `ObjectInfo` from the response
    /// head — the collected stream body must equal the object bytes.
    #[tokio::test]
    async fn get_streaming_streams_object_body() {
        use aws_sdk_s3::{operation::get_object::GetObjectOutput, primitives::ByteStream};

        let get = mock!(aws_sdk_s3::Client::get_object)
            .match_requests(|req| req.key() == Some("folder/obj.bin"))
            .then_output(|| {
                GetObjectOutput::builder()
                    .content_type("application/octet-stream")
                    .content_length(5)
                    .body(ByteStream::from(b"hello".to_vec()))
                    .build()
            });
        let client = mock_client!(aws_sdk_s3, RuleMode::MatchAny, [&get]);
        let svc = service(client);

        let (stream, info) = svc
            .get_streaming("folder", "obj.bin")
            .await
            .expect("get_streaming succeeds");
        assert_eq!(info.key, "obj.bin");
        assert_eq!(info.size, 5);
        assert_eq!(info.content_type, "application/octet-stream");

        let body = stream
            .collect_buffered()
            .await
            .expect("stream ends with a Complete terminal")
            .body;
        assert_eq!(body, b"hello");
        assert_eq!(get.num_calls(), 1);
    }

    /// The 100 MiB streaming cap is enforced up front from the advertised
    /// `Content-Length`: an oversized object is rejected before any bytes
    /// stream (the mocked body is tiny — only the advertised length is large).
    #[tokio::test]
    async fn get_streaming_rejects_object_over_size_cap() {
        use aws_sdk_s3::{operation::get_object::GetObjectOutput, primitives::ByteStream};

        let oversized: i64 = 200 * 1024 * 1024; // past the 100 MiB cap
        let get = mock!(aws_sdk_s3::Client::get_object)
            .match_requests(|req| req.key() == Some("folder/huge.bin"))
            .then_output(move || {
                GetObjectOutput::builder()
                    .content_length(oversized)
                    .body(ByteStream::from(b"x".to_vec()))
                    .build()
            });
        let client = mock_client!(aws_sdk_s3, RuleMode::MatchAny, [&get]);
        let svc = service(client);

        let err = svc
            .get_streaming("folder", "huge.bin")
            .await
            .err()
            .expect("object exceeding the streaming cap must be rejected");
        match err {
            StorageError::Internal(msg) => assert!(
                msg.contains("exceeds streaming limit"),
                "expected size-cap rejection, got: {msg}"
            ),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    /// `delete_folder` streams listing pages and issues one DeleteObjects
    /// batch per page instead of buffering the whole listing first.
    #[tokio::test]
    async fn delete_folder_deletes_in_per_page_batches() {
        let page1 = mock!(aws_sdk_s3::Client::list_objects_v2)
            .match_requests(|req| req.continuation_token().is_none())
            .then_output(|| {
                ListObjectsV2Output::builder()
                    .contents(obj("gone/x", 1))
                    .contents(obj("gone/y", 2))
                    .is_truncated(true)
                    .next_continuation_token("tok1")
                    .build()
            });
        let page2 = mock!(aws_sdk_s3::Client::list_objects_v2)
            .match_requests(|req| req.continuation_token() == Some("tok1"))
            .then_output(|| {
                ListObjectsV2Output::builder()
                    .contents(obj("gone/z", 3))
                    .build()
            });
        let delete_batch_of_two = mock!(aws_sdk_s3::Client::delete_objects)
            .match_requests(|req| req.delete().is_some_and(|d| d.objects().len() == 2))
            .then_output(|| DeleteObjectsOutput::builder().build());
        let delete_batch_of_one = mock!(aws_sdk_s3::Client::delete_objects)
            .match_requests(|req| req.delete().is_some_and(|d| d.objects().len() == 1))
            .then_output(|| DeleteObjectsOutput::builder().build());

        let client = mock_client!(
            aws_sdk_s3,
            RuleMode::MatchAny,
            [&page1, &page2, &delete_batch_of_two, &delete_batch_of_one]
        );
        let svc = service(client);

        svc.delete_folder("gone").await.expect("delete succeeds");
        assert_eq!(delete_batch_of_two.num_calls(), 1);
        assert_eq!(delete_batch_of_one.num_calls(), 1);
    }
}
