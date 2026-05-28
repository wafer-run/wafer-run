//! The Block trait — core interface every WAFER block must implement.

use wafer_block_macro::wafer_async_trait;

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
#[wafer_async_trait]
pub trait Block: crate::compat::MaybeSend + crate::compat::MaybeSync + 'static {
    /// Static metadata describing this block (name, version, routes,
    /// declared config keys, capabilities, etc).
    fn info(&self) -> BlockInfo;

    /// Handle an incoming message. Request body bytes (if any) flow in via `input`.
    /// The returned OutputStream yields zero-or-more Chunk/Meta events then exactly
    /// one terminal event (Complete/Error/Drop/Continue).
    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream;

    /// Lifecycle hook invoked by the runtime for events such as `Init`,
    /// `Migrate`, and `Shutdown`. Default implementation is a no-op.
    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }

    /// Walk this block's config and emit references to other blocks the
    /// config declares. Called once per `block_configs` entry at
    /// `seal()` time so the runtime can detect unresolvable references
    /// before the first request.
    ///
    /// Default returns no references. Blocks whose config holds block
    /// names (router routes, middleware chains, composite expanders)
    /// should override.
    ///
    /// The `config` parameter is the raw JSON the operator wrote — use
    /// `config.get("field")` patterns to navigate it. The runtime
    /// passes a non-null `Value` for every registered block-config
    /// entry.
    fn collect_block_refs(&self, _config: &serde_json::Value) -> Vec<crate::error::BlockConfigRef> {
        Vec::new()
    }

    /// Called after the runtime starts with a handle for running flows/blocks.
    /// The handle is type-erased — downcast to `wafer_run::RuntimeHandle` if needed.
    /// Native-only: wasm32 blocks do not receive bind calls.
    #[cfg(not(target_arch = "wasm32"))]
    fn bind(&self, _handle: Box<dyn std::any::Any + Send + Sync>) {}

    /// Return the capability restrictions for this block, if any.
    /// `None` means unrestricted (native blocks). WASM blocks return `Some(caps)`.
    ///
    /// Returns an owned clone so interior-mutable implementations can read their
    /// current caps without exposing a lifetime-bound guard to callers.
    fn block_capabilities(&self) -> Option<BlockCapabilities> {
        None
    }

    /// Update the block's runtime-enforcement capabilities atomically.
    ///
    /// Called by the runtime after `resolve()` computes effective caps
    /// (`declared ∩ config`). Native blocks ignore this call (default no-op);
    /// WASM blocks override to update their interior-mutable caps field so
    /// every subsequent host-import check uses the effective set.
    fn runtime_capabilities_mut(&self, _new: BlockCapabilities) {
        // Default: no-op. Native blocks are trusted and do not enforce caps.
    }

    /// Declare UI routes this block serves (SSR pages).
    /// The router auto-prefixes each path with `/b/{block_short_name}`.
    fn ui_routes(&self) -> Vec<UiRoute> {
        Vec::new()
    }

    /// Return `self` as `&dyn std::any::Any` for runtime downcasting.
    ///
    /// The default implementation returns `None`. WASM blocks override to
    /// return `Some(self)` so the runtime can downcast `Arc<dyn Block>` to
    /// `Arc<WasmiBlock>` and forward the asset loader without requiring a
    /// separate trait method that imports `wafer-run` types.
    ///
    /// Non-WASM blocks do not need to override this.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
}
