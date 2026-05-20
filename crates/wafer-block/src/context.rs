//! The Context trait — runtime capabilities provided to blocks.

use std::collections::BTreeMap;

use wafer_block_macro::wafer_async_trait;

use crate::{
    core_types::{Attachment, Message, WaferError},
    streams::{
        input::InputStream,
        output::{BufferedResponse, OutputStream},
    },
    types::BlockInfo,
};

/// Context provides runtime capabilities to blocks.
#[wafer_async_trait]
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

    /// Call another block, attaching named binary blobs that the callee can
    /// retrieve via `lookup_attachment`. Caller-attached, per-invocation; the
    /// attachments do not propagate beyond the immediate callee.
    ///
    /// Default impl drops attachments and falls back to `call_block` so
    /// existing native callers compile unchanged. wasmi-backed Context impls
    /// override this to use `__wafer_host_stream_attach`.
    async fn call_block_with_attachments(
        &self,
        block_name: &str,
        msg: Message,
        input: InputStream,
        attachments: BTreeMap<String, Attachment>,
    ) -> OutputStream {
        let _ = attachments;
        self.call_block(block_name, msg, input).await
    }

    /// Buffered variant — analogous to `call_block_buffered`.
    async fn call_block_buffered_with_attachments(
        &self,
        block_name: &str,
        msg: Message,
        body: &[u8],
        attachments: BTreeMap<String, Attachment>,
    ) -> Result<BufferedResponse, WaferError> {
        let input = if body.is_empty() {
            InputStream::empty()
        } else {
            InputStream::from_bytes(body.to_vec())
        };
        let output = self
            .call_block_with_attachments(block_name, msg, input, attachments)
            .await;
        output.collect_buffered().await.map_err(WaferError::from)
    }

    /// Look up an attachment in the current call frame's inbound view.
    /// Returns `None` if the caller did not attach under `id`. Synchronous —
    /// reads from runtime-managed state. Default impl returns `None`.
    fn lookup_attachment(&self, id: &str) -> Option<Attachment> {
        let _ = id;
        None
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

    /// Get an owned `Arc<dyn Context>` from a `&dyn Context`. Concrete
    /// implementations clone their inner Arc-shaped state to produce a
    /// new owning handle. Use this when long-lived service objects need
    /// to retain a Context handle past the lifetime of a borrow (e.g.
    /// AuthServiceImpl populating a OnceLock from its `init` method).
    fn clone_arc(&self) -> std::sync::Arc<dyn Context>;

    /// Validate every registered block's declared `ConfigVar` against the
    /// active config source — same semantics as
    /// `Wafer::validate_all_block_configs`, but callable from inside a
    /// block. Used by deploy-time health endpoints (e.g. wafer-site's
    /// `/_health` route) to surface missing required config keys after
    /// lazy init.
    ///
    /// Default impl returns an empty report. Context impls that don't
    /// run inside the WAFER runtime (test mocks, FFI shims) have no
    /// blocks or config source to walk; they leave this as-is.
    async fn validate_all_block_configs(&self) -> crate::validation::ValidationReport {
        crate::validation::ValidationReport::default()
    }
}
