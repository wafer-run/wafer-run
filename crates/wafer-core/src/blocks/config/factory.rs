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
        Arc::new(super::block::ConfigBlock::new(None))
    }

    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer/config".to_string(),
            version: "0.1.0".to_string(),
            interface: "wafer.infra.config".to_string(),
            summary: "Self-configuring config block factory".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }
}
