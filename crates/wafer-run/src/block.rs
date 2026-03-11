use std::future::Future;
use std::pin::Pin;

use crate::context::Context;
use crate::types::*;
use crate::wasm::capabilities::BlockCapabilities;

// Re-export BlockInfo and AdminUIInfo from wafer-block.
pub use wafer_block::{AdminUIInfo, BlockInfo};

/// Block is the core interface every WAFER block must implement.
///
/// All methods are async to support both sync (standalone server) and
/// async (Cloudflare Workers) execution environments.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait Block: Send + Sync {
    fn info(&self) -> BlockInfo;
    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_;
    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> std::result::Result<(), WaferError>;

    /// Called after the runtime is wrapped in Arc, giving blocks a clonable
    /// handle to execute flows. Only blocks that spawn async tasks (like the
    /// HTTP listener) need to override this.
    #[cfg(not(target_arch = "wasm32"))]
    fn bind(&self, _handle: crate::runtime::RuntimeHandle) {}

    /// Return the capability restrictions for this block, if any.
    /// None means unrestricted (native blocks). WASM blocks return Some(&caps).
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
///
/// For handlers that need to perform async work, use `AsyncFuncBlock` instead.
pub struct FuncBlock {
    pub info: BlockInfo,
    pub handler: Box<dyn Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync>,
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
///
/// Use this when the handler needs to perform async operations such as
/// calling other blocks or performing I/O.
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
