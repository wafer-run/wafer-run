//! The Context trait — runtime capabilities provided to blocks.

use crate::types::BlockInfo;
use crate::{Message, Result_};

/// Context provides runtime capabilities to blocks.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait Context: crate::compat::MaybeSend + crate::compat::MaybeSync {
    /// Call another block by name.
    async fn call_block(&self, block_name: &str, msg: &mut Message) -> Result_;

    /// Check if the context has been cancelled.
    fn is_cancelled(&self) -> bool;

    /// Get a config value from the block's node config.
    fn config_get(&self, key: &str) -> Option<&str>;

    /// List all registered blocks.
    fn registered_blocks(&self) -> Vec<BlockInfo> {
        Vec::new()
    }

    /// List flow summary info.
    fn flow_infos(&self) -> Vec<wafer_flow::FlowInfo> {
        Vec::new()
    }

    /// List full flow definitions.
    fn flow_defs(&self) -> Vec<wafer_flow::WaferFlow> {
        Vec::new()
    }

    /// Get expanded block configs (for inspector app view).
    fn block_configs(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }

    /// List registered interface specifications.
    fn interface_specs(&self) -> Vec<crate::types::InterfaceSpec> {
        Vec::new()
    }
}
