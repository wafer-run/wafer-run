//! S3-compatible storage block — `wafer-run/s3`.
//!
//! Self-contained block wrapping the S3 storage service.
//! Uses the shared storage message handler for the `storage@v1` interface.

#![warn(missing_docs)]

pub mod service;

use std::sync::{Arc, OnceLock};

use service::S3StorageService;
use wafer_block::*;
use wafer_core::interfaces::storage::service::StorageService;

const ENDPOINT_ENV: &str = "WAFER_RUN__S3__ENDPOINT";
const REGION_ENV: &str = "WAFER_RUN__S3__REGION";
const DEFAULT_REGION: &str = "us-east-1";
const DEFAULT_BUCKET: &str = "solobase";

/// The S3-compatible storage block.
///
/// Initialized during `lifecycle(Init)`. Two config namespaces:
/// - Per-flow JSON (declared in `BlockInfo::flow_config`): `bucket`, `prefix`.
///   Each S3 block instance can serve a different bucket / prefix per flow.
/// - Process env (declared in `BlockInfo::config_keys`):
///   `WAFER_RUN__S3__ENDPOINT`, `WAFER_RUN__S3__REGION`.
///   These are typically uniform across flows in a single wafer-run process.
pub(crate) struct S3StorageBlock {
    service: OnceLock<Arc<dyn StorageService>>,
}

impl Default for S3StorageBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl S3StorageBlock {
    /// Construct an uninitialized block; the storage service is built during `lifecycle(Init)`.
    pub(crate) fn new() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for S3StorageBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/s3",
            "0.0.1",
            "storage@v1",
            "S3-compatible storage block",
        )
        .category(BlockCategory::Infrastructure)
        .flow_config(vec![
            ConfigVar::new(
                "bucket",
                "S3 bucket name this block reads from and writes to.",
                DEFAULT_BUCKET,
            )
            .name("Bucket"),
            ConfigVar::new(
                "prefix",
                "Optional key prefix applied to every object stored or fetched.",
                "",
            )
            .name("Prefix"),
        ])
        .config_keys(vec![
            ConfigVar::new(
                ENDPOINT_ENV,
                "S3-compatible endpoint URL (e.g., MinIO). Empty for AWS.",
                "",
            )
            .name("Endpoint"),
            ConfigVar::new(
                REGION_ENV,
                "AWS region used when talking to a non-AWS S3 endpoint.",
                DEFAULT_REGION,
            )
            .name("Region"),
        ])
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let Some(service) = self.service.get() else {
            // Reached if the runtime dispatches a message before lifecycle(Init)
            // completes — programmer error in the host, but surface as a typed
            // error rather than panicking so the requester gets a clean 500.
            return OutputStream::error(WaferError::new(
                ErrorCode::Internal,
                "wafer-run/s3: not initialized — call lifecycle(Init) first",
            ));
        };
        let body = input.collect_to_bytes().await;
        wafer_core::interfaces::storage::handler::handle_message(service.as_ref(), &msg, &body)
            .await
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init && self.service.get().is_none() {
            let config = wafer_block::BlockConfig::from_event(&event);

            // Per-flow JSON (snake_case).
            let bucket = match config.str("bucket") {
                "" => DEFAULT_BUCKET.to_string(),
                s => s.to_string(),
            };
            let prefix = config.str("prefix").to_string();

            // Process env (SCREAMING_SNAKE).
            let endpoint = std::env::var(ENDPOINT_ENV).unwrap_or_default();
            let region = std::env::var(REGION_ENV).unwrap_or_else(|_| DEFAULT_REGION.to_string());

            let svc = if endpoint.is_empty() {
                S3StorageService::new(&bucket, &prefix).await
            } else {
                S3StorageService::with_endpoint(&bucket, &prefix, &endpoint, &region).await
            }
            .map_err(|e| WaferError::new("init", format!("wafer-run/s3: {e}")))?;

            tracing::info!(bucket = %bucket, "S3 storage service initialized");
            self.service.set(Arc::new(svc)).ok();
        }
        Ok(())
    }
}

/// Register the S3 storage block with the given block registry.
pub fn register(w: &mut dyn wafer_block::BlockRegistry) -> Result<(), wafer_block::RuntimeError> {
    w.register_block("wafer-run/s3", Arc::new(S3StorageBlock::new()))
}
