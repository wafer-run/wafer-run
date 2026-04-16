//! The Context trait — runtime capabilities provided to blocks.

use crate::{
    core_types::{Message, WaferError},
    streams::{
        input::InputStream,
        output::{BufferedResponse, OutputStream},
    },
    types::BlockInfo,
};

/// Context provides runtime capabilities to blocks.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait Context: crate::compat::MaybeSend + crate::compat::MaybeSync {
    /// Call another block by name.
    async fn call_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream;

    /// Call another block and collect the full buffered response.
    ///
    /// Convenience wrapper: builds an `InputStream` from `body`, calls the block,
    /// and drains the `OutputStream` into a [`BufferedResponse`]. Returns `Err` if
    /// the stream terminates with anything other than `Complete`.
    async fn call_block_buffered(
        &self,
        block_name: &str,
        msg: Message,
        body: &[u8],
    ) -> Result<BufferedResponse, WaferError> {
        let input = if body.is_empty() {
            InputStream::empty()
        } else {
            InputStream::from_bytes(body.to_vec())
        };
        let output = self.call_block(block_name, msg, input).await;
        output.collect_buffered().await.map_err(WaferError::from)
    }

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

    /// The block name of the caller that invoked this block via `call_block()`.
    /// Returns `None` for top-level calls (e.g. from the router).
    fn caller_id(&self) -> Option<&str> {
        None
    }
}
