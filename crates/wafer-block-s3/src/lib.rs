//! S3-compatible storage block — `wafer-run/s3`.
//!
//! Self-contained block wrapping the S3 storage service.
//! Uses the shared storage message handler for the `storage@v1` interface.

#![warn(missing_docs)]

pub mod service;

use std::sync::Arc;

use service::S3StorageService;
use wafer_block::{BlockConfig, ConfigVar, ErrorCode, InputType, LifecycleType, WaferError};
use wafer_core::interfaces::storage::{
    handler,
    service::{StorageService, DEFAULT_MAX_OBJECT_BYTES},
};

const ENDPOINT_ENV: &str = "WAFER_RUN__S3__ENDPOINT";
const REGION_ENV: &str = "WAFER_RUN__S3__REGION";
const MAX_OBJECT_BYTES_ENV: &str = "WAFER_RUN__S3__MAX_OBJECT_BYTES";
const DEFAULT_REGION: &str = "us-east-1";
const DEFAULT_BUCKET: &str = "wafer";

wafer_core::service_block! {
    /// The S3-compatible storage block.
    ///
    /// Initialized during `lifecycle(Init)`. Two config namespaces:
    /// - Per-flow JSON (declared in `BlockInfo::flow_config`): `bucket`, `prefix`.
    ///   Each S3 block instance can serve a different bucket / prefix per flow.
    /// - Declared config vars (`BlockInfo::config_keys`):
    ///   `WAFER_RUN__S3__ENDPOINT`, `WAFER_RUN__S3__REGION`,
    ///   `WAFER_RUN__S3__MAX_OBJECT_BYTES`, which the runtime resolves through
    ///   the embedder's `ConfigSource` and hands over in the Init payload.
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
            .name("Endpoint")
            .optional(),
            ConfigVar::new(
                REGION_ENV,
                "AWS region used when talking to a non-AWS S3 endpoint.",
                DEFAULT_REGION,
            )
            .name("Region"),
            ConfigVar::new(
                MAX_OBJECT_BYTES_ENV,
                "Largest object, in bytes, a read returns; a larger one fails with \
                 ResourceExhausted. Defaults to 100 MiB (104857600) when unset. Read \
                 once at Init: an invalid value fails Init (restart to apply).",
                &DEFAULT_MAX_OBJECT_BYTES.to_string(),
            )
            .name("Max Object Bytes")
            .input_type(InputType::Number),
        ]),
    handle: |service, _this, ctx, msg, body| {
        handler::handle_message(service.as_ref(), ctx, &msg, &body).await
    },
    // Upload streaming: route `storage.put_streaming` to the streaming handler
    // with the raw `InputStream` intact (no `collect_to_bytes`) so storage@v1
    // stays complete for the S3 backend. `S3StorageService::put_streaming`
    // currently keeps the buffered default — a true streaming S3 upload needs a
    // multipart flow (initiate / upload-part / complete), deferred to a
    // follow-up — but the dispatch reaches it either way.
    stream_ingress: {
        op: wafer_block::common::ServiceOp::STORAGE_PUT_STREAMING,
        handle: |service, _this, ctx, msg, input| {
            handler::handle_put_streaming(service.as_ref(), ctx, &msg, input).await
        },
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

            // Declared config vars (SCREAMING_SNAKE), resolved by the runtime.
            let endpoint = config.str(ENDPOINT_ENV);
            let region = match config.str(REGION_ENV) {
                "" => DEFAULT_REGION,
                region => region,
            };
            let max_object_bytes = max_object_bytes(config.get(MAX_OBJECT_BYTES_ENV))?;

            let svc = if endpoint.is_empty() {
                S3StorageService::new(&bucket, &prefix).await
            } else {
                S3StorageService::with_endpoint(&bucket, &prefix, endpoint, region).await
            }
            .map_err(|e| WaferError::new(ErrorCode::Internal, format!("wafer-run/s3 init: {e}")))?
            .with_max_object_bytes(max_object_bytes);

            tracing::info!(bucket = %bucket, "S3 storage service initialized");
            this.service.set(Arc::new(svc)).ok();
        }
        Ok(())
    },
}

wafer_block::register_static_block!("wafer-run/s3", S3StorageBlock);

/// The read cap from the Init payload's [`MAX_OBJECT_BYTES_ENV`]: unset is
/// [`DEFAULT_MAX_OBJECT_BYTES`]; anything but a positive integer fails Init
/// rather than silently falling back to the default.
fn max_object_bytes(value: Option<&serde_json::Value>) -> Result<u64, WaferError> {
    match value {
        None => Ok(DEFAULT_MAX_OBJECT_BYTES),
        Some(serde_json::Value::String(raw)) => parse_max_object_bytes(raw),
        Some(other) => parse_max_object_bytes(&other.to_string()),
    }
}

/// Parse a set [`MAX_OBJECT_BYTES_ENV`] value: a positive integer.
fn parse_max_object_bytes(raw: &str) -> Result<u64, WaferError> {
    match raw.parse::<u64>() {
        Ok(v) if v > 0 => Ok(v),
        _ => Err(WaferError::new(
            ErrorCode::InvalidArgument,
            format!(
                "wafer-run/s3 init: {MAX_OBJECT_BYTES_ENV}={raw:?} is invalid: expected a \
                 positive integer byte count (unset it to use the default \
                 {DEFAULT_MAX_OBJECT_BYTES})"
            ),
        )),
    }
}

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
        // Denies every access, as the trait's default `check_resource_access` does.
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_block::types::ResourceType,
            _access: wafer_block::types::ResourceAccess,
        ) -> bool {
            false
        }
    }

    #[test]
    fn max_object_bytes_accepts_only_a_positive_integer() {
        assert_eq!(super::parse_max_object_bytes("5").expect("valid"), 5);
        for bad in ["0", "-1", "abc", "", "1.5"] {
            let err = super::parse_max_object_bytes(bad).expect_err(bad);
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{bad}");
        }
    }

    /// The runtime hands resolved config over as strings; a number from
    /// registered block JSON reads the same, and anything else is refused.
    #[test]
    fn max_object_bytes_reads_the_init_payload_value() {
        assert_eq!(
            super::max_object_bytes(None).expect("unset"),
            super::DEFAULT_MAX_OBJECT_BYTES
        );
        assert_eq!(
            super::max_object_bytes(Some(&serde_json::json!("7"))).expect("string"),
            7
        );
        assert_eq!(
            super::max_object_bytes(Some(&serde_json::json!(7))).expect("number"),
            7
        );
        for bad in [serde_json::json!(true), serde_json::json!(["7"])] {
            let err = super::max_object_bytes(Some(&bad)).expect_err("not a count");
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{bad}");
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
