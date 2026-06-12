#![warn(missing_docs)]
//! Read-only guard middleware block for the WAFER runtime.
//!
//! When the `readonly` flow config var is `true`/`1`, this middleware rejects
//! `create`/`update`/`delete` request actions with [`ErrorCode::PermissionDenied`]
//! and lets all other actions (e.g. `retrieve`, `list`) pass through unchanged.

use wafer_block::*;

/// Middleware block that rejects write actions when its flow is configured for read-only mode.
///
/// Registered as `wafer-run/readonly-guard`. Behavior is driven by the `readonly` flow config
/// var; the in-struct `enabled` field is only used as the fallback when no config is provided.
pub struct ReadonlyGuardBlock {
    /// Fallback read-only state used when the flow has no `readonly` config var set.
    enabled: bool,
}

impl Default for ReadonlyGuardBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadonlyGuardBlock {
    /// Construct a new guard with the fallback read-only flag disabled.
    ///
    /// The effective mode at request time is taken from the `readonly` flow config var;
    /// this fallback only applies when that var is absent.
    pub fn new() -> Self {
        Self { enabled: false }
    }
}

#[wafer_async_trait]
impl Block for ReadonlyGuardBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/readonly-guard",
            "0.0.1",
            "middleware@v1",
            "Blocks write operations in read-only mode",
        )
        .infrastructure()
        .flow_config(vec![ConfigVar::new(
            "readonly",
            "When true, the guard rejects create/update/delete actions.",
            "false",
        )
        .name("Read-only")])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let readonly = ctx
            .config_get("readonly")
            .map_or(self.enabled, |s| s == "true" || s == "1");

        if !readonly {
            return OutputStream::continue_with(msg);
        }

        let action = msg.action().to_string();
        if action == RequestAction::CREATE
            || action == RequestAction::UPDATE
            || action == RequestAction::DELETE
        {
            return OutputStream::error(WaferError {
                code: ErrorCode::PermissionDenied,
                message: "This instance is in read-only mode. Write operations are not allowed."
                    .to_string(),
                meta: vec![],
            });
        }

        OutputStream::continue_with(msg)
    }
}

wafer_block::register_static_block!("wafer-run/readonly-guard", ReadonlyGuardBlock);

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use wafer_block::{
        streams::{input::InputStream, output::TerminalNotResponse},
        Message,
    };
    use wafer_test_support::builder::WaferBuilder;

    use super::*;

    async fn build_wafer(config: Option<serde_json::Value>) -> Arc<wafer_run::Wafer> {
        let mut b = WaferBuilder::new().with_block(
            "wafer-run/readonly-guard",
            Arc::new(ReadonlyGuardBlock::new()),
        );
        if let Some(cfg) = config {
            b = b.with_config("wafer-run/readonly-guard", cfg);
        }
        b.build().await.expect("build")
    }

    async fn expect_allowed(wafer: &Arc<wafer_run::Wafer>, action: &str) {
        let mut msg = Message::new(action);
        // Populate META_REQ_ACTION so the action validator picks up the action.
        msg.set_meta("req.action", action);
        match wafer
            .run_block("wafer-run/readonly-guard", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Ok(_) => {} // Respond terminals are allowed (rare for middleware)
            Err(TerminalNotResponse::Continue(_)) => {} // Expected for middleware
            other => panic!("expected allow for action '{action}', got {other:?}"),
        }
    }

    async fn expect_denied(wafer: &Arc<wafer_run::Wafer>, action: &str) {
        let mut msg = Message::new(action);
        msg.set_meta("req.action", action);
        match wafer
            .run_block("wafer-run/readonly-guard", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::PermissionDenied);
            }
            other => panic!("expected PermissionDenied for action '{action}', got {other:?}"),
        }
    }

    #[tokio::test]
    async fn readonly_off_write_actions_allowed() {
        let wafer = build_wafer(Some(json!({"readonly": "false"}))).await;
        expect_allowed(&wafer, RequestAction::CREATE).await;
        expect_allowed(&wafer, RequestAction::UPDATE).await;
        expect_allowed(&wafer, RequestAction::DELETE).await;
    }

    #[tokio::test]
    async fn readonly_on_write_actions_all_deny() {
        let wafer = build_wafer(Some(json!({"readonly": "true"}))).await;
        expect_denied(&wafer, RequestAction::CREATE).await;
        expect_denied(&wafer, RequestAction::UPDATE).await;
        expect_denied(&wafer, RequestAction::DELETE).await;
    }

    #[tokio::test]
    async fn readonly_on_read_actions_allowed() {
        let wafer = build_wafer(Some(json!({"readonly": "true"}))).await;
        expect_allowed(&wafer, RequestAction::RETRIEVE).await;
        expect_allowed(&wafer, "list").await;
    }

    #[tokio::test]
    async fn readonly_default_off_allows_writes() {
        let wafer = build_wafer(None).await;
        expect_allowed(&wafer, RequestAction::CREATE).await;
    }
}
