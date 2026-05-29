//! Runtime — trait abstraction over the WAFER runtime.
//!
//! Bound blocks receive an `Arc<dyn Runtime>` via [`Block::bind`] and use it
//! to dispatch flows / blocks from background tasks. Implementing the trait
//! in `wafer-block` (a leaf types crate) keeps consumers like
//! `wafer-block-http-listener` from depending on the heavyweight `wafer-run`
//! crate just for the dispatch surface.
//!
//! The only production implementer is `wafer_run::RuntimeHandle`.
//!
//! [`Block::bind`]: crate::Block::bind

use wafer_block_macro::wafer_async_trait;

use crate::{
    compat::{MaybeSend, MaybeSync},
    core_types::Message,
    streams::{input::InputStream, output::OutputStream},
};

/// Runtime-level dispatch surface exposed to bound blocks.
///
/// Mirrors the inherent API on `wafer_run::RuntimeHandle`. Implementations
/// run flows and blocks against the live runtime; the trait carries no
/// runtime state of its own.
#[wafer_async_trait]
pub trait Runtime: MaybeSend + MaybeSync + 'static {
    /// Run a registered flow by id.
    async fn run(&self, flow_id: &str, msg: Message, input: InputStream) -> OutputStream;

    /// Run a registered block by name, bypassing flows.
    async fn run_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream;
}
