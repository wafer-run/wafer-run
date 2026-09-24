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
///
/// `get` and `get_streaming` refuse an object larger than the read cap
/// ([`DEFAULT_MAX_OBJECT_BYTES`] unless set with
/// [`Self::with_max_object_bytes`]) with [`StorageError::TooLarge`].
pub struct S3StorageService {
    client: Client,
    bucket: String,
    prefix: String,
    max_object_bytes: u64,
}

impl S3StorageService {
    /// Create with default AWS config (env vars / IAM role).
    pub async fn new(bucket: &str, prefix: &str) -> Result<Self, StorageError> {
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let client = Client::new(&config);
        Ok(Self::from_client(client, bucket, prefix))
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
        Ok(Self::from_client(client, bucket, prefix))
    }

    fn from_client(client: Client, bucket: &str, prefix: &str) -> Self {
        Self {
            client,
            bucket: bucket.to_string(),
            prefix: prefix.to_string(),
            max_object_bytes: DEFAULT_MAX_OBJECT_BYTES,
        }
    }

    /// Set the largest object, in bytes, `get` and `get_streaming` will read.
    #[must_use]
    pub fn with_max_object_bytes(mut self, max_object_bytes: u64) -> Self {
        self.max_object_bytes = max_object_bytes;
        self
    }

    /// Refuse an object whose advertised `Content-Length` is over the read
    /// cap. A missing or negative length passes; the caller bounds the body
    /// by its running total instead.
    fn check_advertised_length(
        &self,
        s3_key: &str,
        content_length: i64,
    ) -> Result<(), StorageError> {
        match u64::try_from(content_length) {
            Ok(advertised) if advertised > self.max_object_bytes => {
                Err(StorageError::TooLarge(format!(
                    "S3 object {s3_key} is {advertised} bytes, exceeds limit of {} bytes",
                    self.max_object_bytes
                )))
            }
            _ => Ok(()),
        }
    }

    /// Issue one `DeleteObjects` request for `keys` and return the keys S3
    /// reported in the response's `Errors` list, with their error codes.
    /// S3 answers `200 OK` even when some keys were not deleted, so an `Ok`
    /// from the SDK alone does not mean the batch is gone.
    async fn delete_batch(&self, keys: &[String]) -> Result<Vec<FailedDelete>, StorageError> {
        let objects = keys
            .iter()
            .map(|k| {
                aws_sdk_s3::types::ObjectIdentifier::builder()
                    .key(k)
                    .build()
                    .map_err(|e| StorageError::Internal(format!("build ObjectIdentifier {k}: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let delete = aws_sdk_s3::types::Delete::builder()
            .set_objects(Some(objects))
            .build()
            .map_err(|e| StorageError::Internal(format!("build Delete request: {e}")))?;

        let output = self
            .client
            .delete_objects()
            .bucket(&self.bucket)
            .delete(delete)
            .send()
            .await
            .map_err(|e| StorageError::Internal(format!("S3 DeleteObjects: {e}")))?;

        Ok(output
            .errors()
            .iter()
            .map(|e| FailedDelete {
                key: e.key().unwrap_or_default().to_string(),
                code: e.code().unwrap_or_default().to_string(),
                message: e.message().unwrap_or_default().to_string(),
            })
            .collect())
    }

    /// Delete `keys`, retrying the keys S3 failed with a transient per-key
    /// error ([`is_transient_delete_error`]), for [`DELETE_ATTEMPTS`]
    /// attempts in all. Returns every key still not deleted after that.
    async fn delete_keys(&self, keys: Vec<String>) -> Result<Vec<FailedDelete>, StorageError> {
        let mut pending = keys;
        let mut not_deleted = Vec::new();
        let mut attempt: u32 = 1;
        loop {
            let (transient, permanent): (Vec<_>, Vec<_>) = self
                .delete_batch(&pending)
                .await?
                .into_iter()
                .partition(|f| !f.key.is_empty() && is_transient_delete_error(&f.code));
            not_deleted.extend(permanent);
            if transient.is_empty() || attempt == DELETE_ATTEMPTS {
                not_deleted.extend(transient);
                return Ok(not_deleted);
            }
            tokio::time::sleep(DELETE_RETRY_BACKOFF * attempt).await;
            attempt += 1;
            pending = transient.into_iter().map(|f| f.key).collect();
        }
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

    /// Cursor-paginated single-page fetch — the cursor arm of [`Self::list`].
    ///
    /// One `ListObjectsV2` request keyed off the opaque continuation token: an
    /// empty `cursor` starts at the first page (no `ContinuationToken` sent), a
    /// non-empty one is fed straight to S3 as the `ContinuationToken`. At most
    /// `limit` keys are requested (S3 caps each page at [`MAX_KEYS_PER_PAGE`]);
    /// `limit == 0` fetches a full S3 page. The folder marker (the zero-length
    /// `{folder}/` object) is skipped, so the first page can yield one fewer
    /// object than `limit`. `next_cursor` is S3's `NextContinuationToken`,
    /// present exactly when the listing is truncated (`None` on the final
    /// page), so consecutive pages neither overlap nor gap. `total_count` is
    /// just this page's object count — a lower bound; the cursor path
    /// deliberately never walks the keyspace to produce an exact total.
    async fn list_by_cursor(
        &self,
        prefix: &str,
        search_prefix: &str,
        cursor: &str,
        limit: i64,
    ) -> Result<ObjectList, StorageError> {
        let mut req = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(search_prefix);
        // A positive limit caps the page size; 0 means "one full S3 page".
        if let Some(l) = usize::try_from(limit).ok().filter(|l| *l > 0) {
            req = req.max_keys(l.min(MAX_KEYS_PER_PAGE) as i32);
        }
        // An empty cursor requests the first page (no ContinuationToken); a
        // non-empty cursor is the opaque token from the previous page.
        if !cursor.is_empty() {
            req = req.continuation_token(cursor);
        }

        let page = req.send().await.map_err(|e| {
            StorageError::Internal(format!("S3 ListObjectsV2 {search_prefix}: {e}"))
        })?;

        let mut objects: Vec<ObjectInfo> = Vec::new();
        for obj in page.contents() {
            let full_key = obj.key().unwrap_or_default();
            let relative_key = full_key
                .strip_prefix(prefix)
                .unwrap_or(full_key)
                .to_string();
            // Skip the folder marker itself (empty key after prefix strip).
            if relative_key.is_empty() {
                continue;
            }
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

        // S3 sets NextContinuationToken exactly when the listing is truncated.
        let next_cursor = page.next_continuation_token().map(str::to_string);
        let total_count = objects.len() as i64;

        Ok(ObjectList {
            objects,
            total_count,
            next_cursor,
        })
    }
}

/// The S3 `ListObjectsV2` per-page key maximum (also the `DeleteObjects`
/// per-request key limit).
const MAX_KEYS_PER_PAGE: usize = 1000;

/// `DeleteObjects` attempts per batch, the first included, when S3 reports
/// transient per-key failures.
const DELETE_ATTEMPTS: u32 = 3;

/// Wait before retry `n` of a batch's transient failures: `n` times this.
const DELETE_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(100);

/// Most failed keys a `delete_folder` error names; the rest are counted.
const MAX_REPORTED_DELETE_FAILURES: usize = 10;

/// One key a `DeleteObjects` response listed under `Errors`.
#[derive(Debug)]
struct FailedDelete {
    key: String,
    code: String,
    message: String,
}

/// Whether a per-key `DeleteObjects` error code is one S3 documents as
/// transient, so the same request can succeed on retry.
fn is_transient_delete_error(code: &str) -> bool {
    matches!(code, "InternalError" | "ServiceUnavailable" | "SlowDown")
}

/// The `delete_folder` error for keys S3 did not delete: the count and the
/// first [`MAX_REPORTED_DELETE_FAILURES`] keys with their codes.
fn partial_delete_error(prefix: &str, failed: &[FailedDelete]) -> StorageError {
    let listed = failed
        .iter()
        .take(MAX_REPORTED_DELETE_FAILURES)
        .map(|f| format!("{} ({}: {})", f.key, f.code, f.message))
        .collect::<Vec<_>>()
        .join(", ");
    let more = failed.len().saturating_sub(MAX_REPORTED_DELETE_FAILURES);
    let tail = if more > 0 {
        format!(", and {more} more")
    } else {
        String::new()
    };
    StorageError::Internal(format!(
        "S3 DeleteObjects {prefix}: {} object(s) not deleted: {listed}{tail}",
        failed.len()
    ))
}

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

    /// Buffers the object body whole, so it enforces the read cap twice: an
    /// advertised `Content-Length` over it is refused before any body byte
    /// is read, and the running total is checked per chunk in case the
    /// length is absent or understated.
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
        self.check_advertised_length(&s3_key, content_length)?;

        let last_modified = resp
            .last_modified()
            .map_or_else(Utc::now, Self::to_chrono_datetime);

        let mut body = Vec::with_capacity(usize::try_from(content_length).unwrap_or(0));
        let mut stream = resp.body;
        while let Some(chunk) = stream
            .next()
            .await
            .transpose()
            .map_err(|e| StorageError::Internal(format!("S3 read body {s3_key}: {e}")))?
        {
            if (body.len() + chunk.len()) as u64 > self.max_object_bytes {
                return Err(StorageError::TooLarge(format!(
                    "S3 object {s3_key} exceeds limit of {} bytes",
                    self.max_object_bytes
                )));
            }
            body.extend_from_slice(&chunk);
        }

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
    /// Enforces the same read cap as `get` so a huge object cannot stream
    /// unbounded into an isolate: an advertised `Content-Length` over the cap
    /// is refused up front with [`StorageError::TooLarge`], and the running
    /// total is checked per chunk (defending against an absent or
    /// underreported length), surfacing a `ResourceExhausted` `Error`
    /// terminal on overflow.
    async fn get_streaming(
        &self,
        folder: &str,
        key: &str,
    ) -> Result<(OutputStream, ObjectInfo), StorageError> {
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

        // A negative/unknown length passes this guard; the running total
        // below still bounds it.
        self.check_advertised_length(&s3_key, content_length)?;
        let max_object_bytes = self.max_object_bytes;

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
                        if received > max_object_bytes {
                            let _ = sink
                                .error(WaferError::new(
                                    ErrorCode::ResourceExhausted,
                                    format!(
                                        "S3 object {s3_key} exceeds limit of {max_object_bytes} bytes"
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

    /// List objects, either offset-paginated (default) or cursor-paginated
    /// (`opts.cursor` set), with pagination pushed down to S3 (PERF-04).
    ///
    /// **Cursor mode** (`opts.cursor == Some`) pages forward using S3's own
    /// `ContinuationToken`, so a deep page costs a single round trip with no
    /// re-walk of the preceding keyspace. An empty token means "first page"
    /// (no `ContinuationToken` sent); a non-empty token is the opaque
    /// `NextContinuationToken` from the previous page. `offset` is ignored.
    /// The returned `next_cursor` is S3's `NextContinuationToken` — present
    /// exactly when the listing is truncated, `None` on the final page. In
    /// this mode `total_count` is only the current page's object count (a
    /// lower bound); callers use `next_cursor` as the has-more signal. See
    /// [`Self::list_by_cursor`].
    ///
    /// **Offset mode** (`opts.cursor == None`) is unchanged: pages are walked
    /// via continuation tokens and the scan stops as soon as `offset + limit`
    /// objects (plus one peek object) have been seen — the old implementation
    /// buffered *every* page before slicing the window in memory. Each request
    /// also caps `MaxKeys` to what the scan still needs, so the final page is
    /// not a full 1000-key fetch. `total_count` is exact when the scan reached
    /// the end of the listing; when it stopped early it is a lower bound of
    /// `offset + limit + 1` — strictly greater than `offset + limit`, so
    /// "more pages exist" checks (`total_count > offset + limit`) remain
    /// correct. S3 has no way to report an exact total without walking the
    /// whole keyspace. `next_cursor` is always `None` in offset mode.
    async fn list(&self, folder: &str, opts: &ListOptions) -> Result<ObjectList, StorageError> {
        let prefix = self.folder_prefix(folder);
        let search_prefix = if opts.prefix.is_empty() {
            prefix.clone()
        } else {
            format!("{}{}", prefix, opts.prefix)
        };

        // Cursor takes precedence over offset (documented on `ListOptions`).
        if let Some(cursor) = &opts.cursor {
            return self
                .list_by_cursor(&prefix, &search_prefix, cursor, opts.limit)
                .await;
        }

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
            // Offset mode does not emit a cursor: the scan can stop mid-page,
            // so there is no page-boundary continuation token that would let a
            // caller resume without a gap or overlap.
            next_cursor: None,
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

    /// Deletes every object under the folder, marker included. S3 reports
    /// per-key failures inside a `200 OK` `DeleteObjects` response; keys that
    /// failed transiently are retried, and any key still not deleted makes
    /// this return `Err` naming them, so a caller never treats a folder with
    /// surviving objects as gone. The remaining pages are still deleted
    /// before the error is returned; deleting again finishes the job.
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

        let mut not_deleted = Vec::new();
        while let Some(page) = stream.next().await {
            let page = page
                .map_err(|e| StorageError::Internal(format!("S3 list for delete {prefix}: {e}")))?;
            let keys: Vec<String> = page
                .contents()
                .iter()
                .filter_map(|obj| obj.key().map(str::to_string))
                .collect();
            if !keys.is_empty() {
                not_deleted.extend(self.delete_keys(keys).await?);
            }
        }

        if not_deleted.is_empty() {
            Ok(())
        } else {
            Err(partial_delete_error(&prefix, &not_deleted))
        }
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
        S3StorageService::from_client(client, "bucket", "")
    }

    /// A `DeleteObjects` response `Errors` entry.
    fn delete_error(key: &str, code: &str) -> aws_sdk_s3::types::Error {
        aws_sdk_s3::types::Error::builder()
            .key(key)
            .code(code)
            .message(format!("{code} for {key}"))
            .build()
    }

    /// One listing page holding `gone/x` and `gone/y`.
    fn two_key_page() -> aws_smithy_mocks::Rule {
        mock!(aws_sdk_s3::Client::list_objects_v2).then_output(|| {
            ListObjectsV2Output::builder()
                .contents(obj("gone/x", 1))
                .contents(obj("gone/y", 2))
                .build()
        })
    }

    /// The keys a `DeleteObjects` request asks to delete.
    fn requested_keys(
        req: &aws_sdk_s3::operation::delete_objects::DeleteObjectsInput,
    ) -> Vec<&str> {
        req.delete()
            .map(|d| d.objects().iter().map(|o| o.key()).collect())
            .unwrap_or_default()
    }

    fn obj(key: &str, size: i64) -> Object {
        Object::builder().key(key).size(size).build()
    }

    /// `put_streaming` into S3 (the trait default: collect, then
    /// `PutObject`) with a body that fails after its first chunk returns the
    /// body's error and never uploads the prefix.
    #[tokio::test]
    async fn put_streaming_with_a_failing_body_uploads_nothing() {
        use wafer_block::InputStream;

        let put = mock!(aws_sdk_s3::Client::put_object)
            .then_output(|| aws_sdk_s3::operation::put_object::PutObjectOutput::builder().build());
        let svc = service(mock_client!(aws_sdk_s3, RuleMode::MatchAny, [&put]));
        let failure = WaferError::new(ErrorCode::DeadlineExceeded, "request body timed out");
        let body = InputStream::from_stream(futures::stream::iter(vec![
            Ok(b"first half".to_vec()),
            Err(failure.clone()),
        ]));

        match svc.put_streaming("f", "k", body, "text/plain").await {
            Err(StorageError::Body(e)) => assert_eq!(e, failure),
            other => panic!("expected the body's failure, got {other:?}"),
        }
        assert_eq!(put.num_calls(), 0, "a truncated body must not be uploaded");
    }

    /// Positive control: a whole body is uploaded once, whole.
    #[tokio::test]
    async fn put_streaming_with_a_whole_body_uploads_it() {
        use wafer_block::InputStream;

        let put = mock!(aws_sdk_s3::Client::put_object)
            .match_requests(|req| req.key() == Some("f/k"))
            .then_output(|| aws_sdk_s3::operation::put_object::PutObjectOutput::builder().build());
        let svc = service(mock_client!(aws_sdk_s3, RuleMode::MatchAny, [&put]));
        let body = InputStream::from_stream(futures::stream::iter(vec![
            Ok(b"first half, ".to_vec()),
            Ok(b"second half".to_vec()),
        ]));

        svc.put_streaming("f", "k", body, "text/plain")
            .await
            .expect("a whole body is stored");
        assert_eq!(put.num_calls(), 1);
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
                    cursor: None,
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
                    cursor: None,
                },
            )
            .await
            .expect("list succeeds");

        assert!(list.objects.is_empty());
        assert_eq!(list.total_count, 3);
    }

    /// Cursor mode: an empty cursor fetches the first page and returns S3's
    /// `NextContinuationToken` as `next_cursor`; feeding that token back
    /// returns the following objects with no overlap and no gap; the final
    /// page reports `next_cursor: None`. `offset` is ignored throughout.
    #[tokio::test]
    async fn list_by_cursor_pages_forward_without_overlap_or_gap() {
        // Page 1: empty cursor → no ContinuationToken, MaxKeys = limit (2).
        let page1 = mock!(aws_sdk_s3::Client::list_objects_v2)
            .match_requests(|req| {
                req.continuation_token().is_none()
                    && req.max_keys() == Some(2)
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
        // Page 2: cursor = "tok1" → ContinuationToken forwarded, MaxKeys = 2.
        // Listing ends here (no next token) → next_cursor is None.
        let page2 = mock!(aws_sdk_s3::Client::list_objects_v2)
            .match_requests(|req| {
                req.continuation_token() == Some("tok1") && req.max_keys() == Some(2)
            })
            .then_output(|| {
                ListObjectsV2Output::builder()
                    .contents(obj("folder/c", 3))
                    .build()
            });

        let client = mock_client!(aws_sdk_s3, RuleMode::MatchAny, [&page1, &page2]);
        let svc = service(client);

        // Page 1 — bootstrap with an empty cursor. `offset` is deliberately
        // large to prove cursor mode ignores it.
        let p1 = svc
            .list(
                "folder",
                &ListOptions {
                    prefix: String::new(),
                    limit: 2,
                    offset: 999,
                    cursor: Some(String::new()),
                },
            )
            .await
            .expect("cursor page 1 succeeds");
        let p1_keys: Vec<&str> = p1.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(p1_keys, vec!["a", "b"]);
        assert_eq!(
            p1.next_cursor.as_deref(),
            Some("tok1"),
            "a truncated page must surface S3's NextContinuationToken"
        );

        // Page 2 — resume from the token page 1 handed back.
        let p2 = svc
            .list(
                "folder",
                &ListOptions {
                    prefix: String::new(),
                    limit: 2,
                    offset: 0,
                    cursor: p1.next_cursor.clone(),
                },
            )
            .await
            .expect("cursor page 2 succeeds");
        let p2_keys: Vec<&str> = p2.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(p2_keys, vec!["c"], "no overlap with page 1, no gap");
        assert_eq!(
            p2.next_cursor, None,
            "the final page must report no continuation token"
        );
        assert_eq!(page1.num_calls(), 1);
        assert_eq!(page2.num_calls(), 1);
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
            StorageError::TooLarge(msg) => assert!(
                msg.contains("exceeds limit"),
                "expected size-cap rejection, got: {msg}"
            ),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    /// Buffered `get` refuses an object whose advertised `Content-Length` is
    /// over the cap before reading its body (the mocked body is tiny — only
    /// the advertised length is large).
    #[tokio::test]
    async fn get_rejects_advertised_length_over_cap() {
        use aws_sdk_s3::{operation::get_object::GetObjectOutput, primitives::ByteStream};

        let oversized =
            i64::try_from(DEFAULT_MAX_OBJECT_BYTES).expect("cap fits i64") + 1024 * 1024;
        let get = mock!(aws_sdk_s3::Client::get_object).then_output(move || {
            GetObjectOutput::builder()
                .content_length(oversized)
                .body(ByteStream::from(b"x".to_vec()))
                .build()
        });
        let svc = service(mock_client!(aws_sdk_s3, RuleMode::MatchAny, [&get]));

        match svc.get("folder", "huge.bin").await {
            Err(StorageError::TooLarge(msg)) => {
                assert!(msg.contains("folder/huge.bin"), "names the object: {msg}");
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    /// Buffered `get` bounds the body by its running total, so an object whose
    /// `Content-Length` understates the body cannot be read past the cap.
    #[tokio::test]
    async fn get_rejects_body_over_cap_despite_understated_length() {
        use aws_sdk_s3::{operation::get_object::GetObjectOutput, primitives::ByteStream};

        let get = mock!(aws_sdk_s3::Client::get_object).then_output(|| {
            GetObjectOutput::builder()
                .content_length(3)
                .body(ByteStream::from(b"hello".to_vec()))
                .build()
        });
        let svc =
            service(mock_client!(aws_sdk_s3, RuleMode::MatchAny, [&get])).with_max_object_bytes(4);

        match svc.get("folder", "obj.bin").await {
            Err(StorageError::TooLarge(_)) => {}
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    /// An object exactly at the cap reads in full.
    #[tokio::test]
    async fn get_returns_object_at_cap() {
        use aws_sdk_s3::{operation::get_object::GetObjectOutput, primitives::ByteStream};

        let get = mock!(aws_sdk_s3::Client::get_object).then_output(|| {
            GetObjectOutput::builder()
                .content_length(5)
                .body(ByteStream::from(b"hello".to_vec()))
                .build()
        });
        let svc =
            service(mock_client!(aws_sdk_s3, RuleMode::MatchAny, [&get])).with_max_object_bytes(5);

        let (body, info) = svc.get("folder", "obj.bin").await.expect("get succeeds");
        assert_eq!(body, b"hello");
        assert_eq!(info.size, 5);
    }

    /// S3 answers `DeleteObjects` with `200 OK` and lists the keys it did not
    /// delete under `Errors`. A permanent per-key failure must fail
    /// `delete_folder`, name the key and code, and not be retried.
    #[tokio::test]
    async fn delete_folder_fails_on_per_key_error_in_ok_response() {
        let page = two_key_page();
        let delete = mock!(aws_sdk_s3::Client::delete_objects).then_output(|| {
            DeleteObjectsOutput::builder()
                .deleted(
                    aws_sdk_s3::types::DeletedObject::builder()
                        .key("gone/x")
                        .build(),
                )
                .errors(delete_error("gone/y", "AccessDenied"))
                .build()
        });
        let svc = service(mock_client!(
            aws_sdk_s3,
            RuleMode::MatchAny,
            [&page, &delete]
        ));

        match svc.delete_folder("gone").await {
            Err(StorageError::Internal(msg)) => {
                assert!(msg.contains("1 object(s) not deleted"), "{msg}");
                assert!(msg.contains("gone/y (AccessDenied"), "{msg}");
            }
            other => panic!("expected a partial-delete error, got {other:?}"),
        }
        assert_eq!(delete.num_calls(), 1, "a permanent failure is not retried");
    }

    /// A transient per-key failure is retried with only the failed keys, and
    /// `delete_folder` succeeds once the retry deletes them.
    #[tokio::test]
    async fn delete_folder_retries_transient_per_key_errors() {
        let page = two_key_page();
        let first = mock!(aws_sdk_s3::Client::delete_objects)
            .match_requests(|req| requested_keys(req) == ["gone/x", "gone/y"])
            .then_output(|| {
                DeleteObjectsOutput::builder()
                    .errors(delete_error("gone/y", "SlowDown"))
                    .build()
            });
        let retry = mock!(aws_sdk_s3::Client::delete_objects)
            .match_requests(|req| requested_keys(req) == ["gone/y"])
            .then_output(|| DeleteObjectsOutput::builder().build());
        let svc = service(mock_client!(
            aws_sdk_s3,
            RuleMode::MatchAny,
            [&page, &first, &retry]
        ));

        svc.delete_folder("gone")
            .await
            .expect("the retry deletes the rest");
        assert_eq!(first.num_calls(), 1);
        assert_eq!(retry.num_calls(), 1, "only the failed key is retried");
    }

    /// A transient failure that persists through every attempt fails
    /// `delete_folder` after [`DELETE_ATTEMPTS`] requests.
    #[tokio::test]
    async fn delete_folder_fails_when_transient_errors_persist() {
        let page = two_key_page();
        let delete = mock!(aws_sdk_s3::Client::delete_objects)
            .match_requests(|req| requested_keys(req).contains(&"gone/y"))
            .then_output(|| {
                DeleteObjectsOutput::builder()
                    .errors(delete_error("gone/y", "InternalError"))
                    .build()
            });
        let svc = service(mock_client!(
            aws_sdk_s3,
            RuleMode::MatchAny,
            [&page, &delete]
        ));

        match svc.delete_folder("gone").await {
            Err(StorageError::Internal(msg)) => {
                assert!(msg.contains("gone/y (InternalError"), "{msg}");
            }
            other => panic!("expected a partial-delete error, got {other:?}"),
        }
        assert_eq!(delete.num_calls(), DELETE_ATTEMPTS as usize);
    }

    /// The error names at most [`MAX_REPORTED_DELETE_FAILURES`] keys and
    /// counts the rest.
    #[test]
    fn partial_delete_error_caps_the_listed_keys() {
        let failed: Vec<FailedDelete> = (0..12)
            .map(|i| FailedDelete {
                key: format!("k{i}"),
                code: "AccessDenied".into(),
                message: "denied".into(),
            })
            .collect();
        let StorageError::Internal(msg) = partial_delete_error("p/", &failed) else {
            panic!("expected Internal");
        };
        assert!(msg.contains("12 object(s) not deleted"), "{msg}");
        assert!(msg.contains("k9 (AccessDenied: denied)"), "{msg}");
        assert!(!msg.contains("k10 "), "{msg}");
        assert!(msg.ends_with(", and 2 more"), "{msg}");
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
