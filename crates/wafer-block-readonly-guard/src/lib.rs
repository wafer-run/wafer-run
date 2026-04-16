use std::sync::Arc;
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

pub fn register(w: &mut dyn wafer_block::BlockRegistry) -> Result<(), wafer_block::RuntimeError> {
    w.register_block(
        "wafer-run/readonly-guard",
        Arc::new(ReadonlyGuardBlock::new()),
    )
}
