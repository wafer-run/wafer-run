use std::sync::Arc;
use wafer_run::*;

/// ReadonlyGuardBlock blocks write operations when in read-only mode.
pub struct ReadonlyGuardBlock {
    enabled: bool,
}

impl ReadonlyGuardBlock {
    pub fn new() -> Self {
        Self { enabled: false }
    }
}

impl Block for ReadonlyGuardBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/readonly-guard".to_string(),
            version: "0.1.0".to_string(),
            interface: "middleware@v1".to_string(),
            summary: "Blocks write operations in read-only mode".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }

    fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let readonly = ctx
            .config_get("readonly")
            .map(|s| s == "true" || s == "1")
            .unwrap_or(self.enabled);

        if !readonly {
            return msg.clone().cont();
        }

        let action = msg.action();
        if action == "create" || action == "update" || action == "delete" {
            return err_forbidden(
                msg.clone(),
                "This instance is in read-only mode. Write operations are not allowed.",
            );
        }

        msg.clone().cont()
    }

    fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

pub fn register(w: &mut Wafer) {
    w.register_block("@wafer/readonly-guard", Arc::new(ReadonlyGuardBlock::new()));
}
