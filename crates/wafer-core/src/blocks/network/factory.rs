//! BlockFactory for the network block.

/// NetworkBlockFactory creates a NetworkBlock with an HTTP client.
///
/// No config needed.
///
/// Env var overrides: none
#[cfg(feature = "network")]
pub struct NetworkBlockFactory;

#[cfg(feature = "network")]
impl wafer_run::registry::BlockFactory for NetworkBlockFactory {
    fn create(&self, _config: Option<&serde_json::Value>) -> std::sync::Arc<dyn wafer_run::block::Block> {
        use std::sync::Arc;

        let svc = super::service::HttpNetworkService::new();
        Arc::new(super::block::NetworkBlock::new(Arc::new(svc)))
    }

    fn info(&self) -> wafer_run::block::BlockInfo {
        wafer_run::block::BlockInfo {
            name: "@wafer/network".to_string(),
            version: "0.1.0".to_string(),
            interface: "network@v1".to_string(),
            summary: "Network block factory".to_string(),
            instance_mode: wafer_run::types::InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Native,
            requires: Vec::new(),
        }
    }
}
