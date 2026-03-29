//! The Block trait — core interface every WAFER block must implement.

use std::future::Future;
use std::pin::Pin;

use crate::capabilities::BlockCapabilities;
use crate::context::Context;
use crate::types::BlockInfo;
use crate::{LifecycleEvent, Message, Result_, WaferError};

/// Block is the core interface every WAFER block must implement.
///
/// All methods are async to support both sync (standalone server) and
/// async (Cloudflare Workers) execution environments.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg(not(target_arch = "wasm32"))]
pub trait Block: Send + Sync {
    fn info(&self) -> BlockInfo;
    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_;
    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> std::result::Result<(), WaferError>;

    /// Called after the runtime starts with a handle for running flows/blocks.
    /// The handle is type-erased — downcast to `wafer_run::RuntimeHandle` if needed.
    fn bind(&self, _handle: Box<dyn std::any::Any + Send + Sync>) {}

    /// Return the capability restrictions for this block, if any.
    /// None means unrestricted (native blocks). WASM blocks return Some(&caps).
    fn block_capabilities(&self) -> Option<&BlockCapabilities> {
        None
    }
}

/// On wasm32, Send/Sync are not meaningful (single-threaded), so we drop all bounds.
#[async_trait::async_trait(?Send)]
#[cfg(target_arch = "wasm32")]
pub trait Block {
    fn info(&self) -> BlockInfo;
    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_;
    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> std::result::Result<(), WaferError>;

    /// Return the capability restrictions for this block, if any.
    fn block_capabilities(&self) -> Option<&BlockCapabilities> {
        None
    }
}

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
    dyn for<'a> Fn(
            &'a dyn Context,
            &'a mut Message,
        ) -> Pin<Box<dyn Future<Output = Result_> + 'a>>
        + Sync,
>;

/// FuncBlock wraps a synchronous handler function as a Block.
#[cfg(not(target_arch = "wasm32"))]
pub struct FuncBlock {
    pub info: BlockInfo,
    pub handler: Box<dyn Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync>,
}

#[cfg(target_arch = "wasm32")]
pub struct FuncBlock {
    pub info: BlockInfo,
    pub handler: Box<dyn Fn(&dyn Context, &mut Message) -> Result_>,
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
