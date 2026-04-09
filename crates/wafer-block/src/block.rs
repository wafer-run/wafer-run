//! The Block trait — core interface every WAFER block must implement.

use std::future::Future;
use std::pin::Pin;

use crate::capabilities::BlockCapabilities;
use crate::context::Context;
use crate::types::{BlockInfo, UiRoute};
use crate::{LifecycleEvent, Message, Result_, WaferError};

/// Block is the core interface every WAFER block must implement.
///
/// All methods are async to support both sync (standalone server) and
/// async (Cloudflare Workers) execution environments.
///
/// On native targets, requires Send + Sync (via MaybeSend/MaybeSync).
/// On wasm32, these bounds are dropped (single-threaded).
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait Block: crate::compat::MaybeSend + crate::compat::MaybeSync {
    fn info(&self) -> BlockInfo;
    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_;
    async fn lifecycle(
        &self,
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError>;

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

// --- Handler type aliases (cfg-gated for Send+Sync) ---

#[cfg(not(target_arch = "wasm32"))]
type SyncHandler = Box<dyn Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync>;

#[cfg(target_arch = "wasm32")]
type SyncHandler = Box<dyn Fn(&dyn Context, &mut Message) -> Result_>;

/// The async handler type used by `AsyncFuncBlock`.
#[cfg(not(target_arch = "wasm32"))]
type AsyncHandler = Box<
    dyn for<'a> Fn(
            &'a dyn Context,
            &'a mut Message,
        ) -> Pin<Box<dyn Future<Output = Result_> + Send + 'a>>
        + Send
        + Sync,
>;

#[cfg(target_arch = "wasm32")]
type AsyncHandler = Box<
    dyn for<'a> Fn(&'a dyn Context, &'a mut Message) -> Pin<Box<dyn Future<Output = Result_> + 'a>>
        + Sync,
>;

/// FuncBlock wraps a synchronous handler function as a Block.
pub struct FuncBlock {
    pub info: BlockInfo,
    #[allow(clippy::type_complexity)]
    pub handler: SyncHandler,
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for FuncBlock {
    fn info(&self) -> BlockInfo {
        self.info.clone()
    }

    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        (self.handler)(ctx, msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

/// AsyncFuncBlock wraps an async handler function as a Block.
pub struct AsyncFuncBlock {
    pub info: BlockInfo,
    pub handler: AsyncHandler,
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for AsyncFuncBlock {
    fn info(&self) -> BlockInfo {
        self.info.clone()
    }

    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        (self.handler)(ctx, msg).await
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}
