use std::sync::Arc;

use wafer_run::block::{Block, BlockInfo};
use wafer_run::registry::BlockFactory;
use wafer_run::types::InstanceMode;

/// LoggerBlockFactory creates a LoggerBlock.
///
/// No config needed.
pub struct LoggerBlockFactory;

impl BlockFactory for LoggerBlockFactory {
    fn create(&self, _config: Option<&serde_json::Value>) -> Arc<dyn Block> {
        Arc::new(super::block::LoggerBlock::new())
    }

    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer/logger".to_string(),
            version: "0.1.0".to_string(),
            interface: "wafer.infra.logger".to_string(),
            summary: "Self-configuring logger block factory".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }
}
