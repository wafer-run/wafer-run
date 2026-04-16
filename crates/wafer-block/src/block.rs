//! The Block trait — core interface every WAFER block must implement.

use crate::{
    capabilities::BlockCapabilities,
    context::Context,
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    types::{BlockInfo, UiRoute},
};

/// Block is the core interface every WAFER block must implement.
///
/// All methods are async to support both sync (standalone server) and
/// async (Cloudflare Workers) execution environments.
///
/// On native targets, requires Send + Sync (via MaybeSend/MaybeSync).
/// On wasm32, these bounds are dropped (single-threaded).
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait Block: crate::compat::MaybeSend + crate::compat::MaybeSync + 'static {
    fn info(&self) -> BlockInfo;

    /// Handle an incoming message. Request body bytes (if any) flow in via `input`.
    /// The returned OutputStream yields zero-or-more Chunk/Meta events then exactly
    /// one terminal event (Complete/Error/Drop/Continue).
    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream;

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }

    /// Called after the runtime starts with a handle for running flows/blocks.
    /// The handle is type-erased — downcast to `wafer_run::RuntimeHandle` if needed.
    /// Native-only: wasm32 blocks do not receive bind calls.
    #[cfg(not(target_arch = "wasm32"))]
    fn bind(&self, _handle: Box<dyn std::any::Any + Send + Sync>) {}

    /// Return the capability restrictions for this block, if any.
    /// None means unrestricted (native blocks). WASM blocks return Some(&caps).
    fn block_capabilities(&self) -> Option<&BlockCapabilities> {
        None
    }

    /// Declare UI routes this block serves (SSR pages).
    /// The router auto-prefixes each path with `/b/{block_short_name}`.
    fn ui_routes(&self) -> Vec<UiRoute> {
        Vec::new()
    }
}
