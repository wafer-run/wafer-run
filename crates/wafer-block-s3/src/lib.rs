//! S3-compatible storage block — `wafer-run/s3`.
//!
//! Self-contained block wrapping the S3 storage service.
//! Uses the shared storage message handler for the `storage@v1` interface.

#![warn(missing_docs)]

pub mod service;

use std::sync::Arc;

use service::S3StorageService;
use wafer_block::{BlockConfig, ConfigVar, ErrorCode, LifecycleType, WaferError};
use wafer_core::interfaces::storage::{handler, service::StorageService};

const ENDPOINT_ENV: &str = "WAFER_RUN__S3__ENDPOINT";
const REGION_ENV: &str = "WAFER_RUN__S3__REGION";
const DEFAULT_REGION: &str = "us-east-1";
const DEFAULT_BUCKET: &str = "wafer";

wafer_core::service_block! {
    /// The S3-compatible storage block.
    ///
    /// Initialized during `lifecycle(Init)`. Two config namespaces:
    /// - Per-flow JSON (declared in `BlockInfo::flow_config`): `bucket`, `prefix`.
    ///   Each S3 block instance can serve a different bucket / prefix per flow.
    /// - Process env (declared in `BlockInfo::config_keys`):
    ///   `WAFER_RUN__S3__ENDPOINT`, `WAFER_RUN__S3__REGION`.
    ///   These are typically uniform across flows in a single wafer-run process.
    lazy block: pub(crate) S3StorageBlock,
    name: "wafer-run/s3",
    version: "0.0.1",
    interface: "storage@v1",
    description: "S3-compatible storage block",
    category: Infrastructure,
    service: dyn StorageService,
    info_extras: |_this, info| info
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
        ]),
    handle: |service, _this, ctx, msg, body| {
        handler::handle_message(service.as_ref(), ctx, &msg, &body).await
    },
    lifecycle: |this, _ctx, event| {
        if event.event_type == LifecycleType::Init && this.service.get().is_none() {
            let config = BlockConfig::from_event(&event);

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
            .map_err(|e| WaferError::new(ErrorCode::Internal, format!("wafer-run/s3 init: {e}")))?;

            tracing::info!(bucket = %bucket, "S3 storage service initialized");
            this.service.set(Arc::new(svc)).ok();
        }
        Ok(())
    },
}

wafer_block::register_static_block!("wafer-run/s3", S3StorageBlock);

#[cfg(test)]
mod tests {
    use wafer_block::{
        Block, Context, ErrorCode, InputStream, Message, OutputStream, TerminalNotResponse,
    };

    use super::S3StorageBlock;

    /// Minimal `Context` that panics if the block reaches back into the runtime.
    #[derive(Clone)]
    struct NoopContext;

    #[async_trait::async_trait]
    impl Context for NoopContext {
        async fn call_block(
            &self,
            block_name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            panic!("NoopContext::call_block called unexpectedly: block_name={block_name}");
        }

        fn is_cancelled(&self) -> bool {
            false
        }

        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }

        fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    #[tokio::test]
    async fn handle_before_init_yields_typed_error_not_panic() {
        let block = S3StorageBlock::new();
        let out = block
            .handle(
                &NoopContext,
                Message::new("storage.get"),
                InputStream::empty(),
            )
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Internal);
                assert!(
                    e.message.contains("wafer-run/s3") && e.message.contains("not initialized"),
                    "unexpected pre-Init error message: {}",
                    e.message
                );
            }
            other => panic!("expected typed pre-Init error terminal, got: {other:?}"),
        }
    }
}
