use wafer_block::*;

/// ReadonlyGuardBlock blocks write operations when in read-only mode.
pub struct ReadonlyGuardBlock {
    enabled: bool,
}

impl Default for ReadonlyGuardBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadonlyGuardBlock {
    pub fn new() -> Self {
        Self { enabled: false }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for ReadonlyGuardBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/readonly-guard",
            "0.0.1",
            "middleware@v1",
            "Blocks write operations in read-only mode",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
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
            .map(|s| s == "true" || s == "1")
            .unwrap_or(self.enabled);

        if !readonly {
            return OutputStream::continue_with(msg);
        }

        let action = msg.action().to_string();
        if action == "create" || action == "update" || action == "delete" {
            return OutputStream::error(WaferError {
                code: ErrorCode::PermissionDenied,
                message: "This instance is in read-only mode. Write operations are not allowed."
                    .to_string(),
                meta: vec![],
            });
        }

        OutputStream::continue_with(msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

wafer_run::register_static_block!("wafer-run/readonly-guard", ReadonlyGuardBlock);

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
                assert_eq!(e.code, ErrorCode::PERMISSION_DENIED);
            }
            other => panic!("expected PermissionDenied for action '{action}', got {other:?}"),
        }
    }

    #[tokio::test]
    async fn readonly_off_write_actions_allowed() {
        let wafer = build_wafer(Some(json!({"readonly": "false"}))).await;
        expect_allowed(&wafer, "create").await;
        expect_allowed(&wafer, "update").await;
        expect_allowed(&wafer, "delete").await;
    }

    #[tokio::test]
    async fn readonly_on_write_actions_all_deny() {
        let wafer = build_wafer(Some(json!({"readonly": "true"}))).await;
        expect_denied(&wafer, "create").await;
        expect_denied(&wafer, "update").await;
        expect_denied(&wafer, "delete").await;
    }

    #[tokio::test]
    async fn readonly_on_read_actions_allowed() {
        let wafer = build_wafer(Some(json!({"readonly": "true"}))).await;
        expect_allowed(&wafer, "retrieve").await;
        expect_allowed(&wafer, "list").await;
    }

    #[tokio::test]
    async fn readonly_default_off_allows_writes() {
        let wafer = build_wafer(None).await;
        expect_allowed(&wafer, "create").await;
    }
}
