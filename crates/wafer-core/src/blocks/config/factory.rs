use std::sync::Arc;

use wafer_run::block::{Block, BlockInfo};
use wafer_run::registry::BlockFactory;
use wafer_run::types::InstanceMode;

/// ConfigBlockFactory creates a ConfigBlock with env-var-only config reader.
///
/// No config needed.
pub struct ConfigBlockFactory;

impl BlockFactory for ConfigBlockFactory {
    fn create(&self, _config: Option<&serde_json::Value>) -> Arc<dyn Block> {
        Arc::new(super::block::ConfigBlock::new(Some(Arc::new(
            super::service::EnvConfigService::new(),
        ))))
    }

    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/config".to_string(),
            version: "0.1.0".to_string(),
            interface: "config@v1".to_string(),
            summary: "Config block factory".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Wasm,
            requires: Vec::new(),
        }
    }
}
