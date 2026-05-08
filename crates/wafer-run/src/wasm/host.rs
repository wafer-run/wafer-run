use std::{collections::BTreeMap, sync::Arc};

use wafer_block::{
    core_types::Attachment,
    streams::{input::InputStream, output::OutputStream},
    Message,
};

use crate::context::Context;

/// ContextGuard — scoped wrapper for passing a borrowed Context into wasmi.
pub(crate) struct ContextGuard {
    wrapper: Arc<ContextWrapper>,
}

impl ContextGuard {
    pub fn new(ctx: &dyn Context) -> Self {
        let ptr: *const dyn Context = ctx;
        let ptr_static: *const (dyn Context + 'static) = unsafe { std::mem::transmute(ptr) };
        Self {
            wrapper: Arc::new(ContextWrapper(ptr_static)),
        }
    }

    pub fn as_arc(&self) -> Arc<dyn Context> {
        self.wrapper.clone()
    }
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        debug_assert_eq!(
            Arc::strong_count(&self.wrapper),
            1,
            "BUG: ContextGuard dropped while cloned Arcs still exist"
        );
    }
}

struct ContextWrapper(*const dyn Context);
unsafe impl Send for ContextWrapper {}
unsafe impl Sync for ContextWrapper {}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Context for ContextWrapper {
    async fn call_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream {
        unsafe { &*self.0 }.call_block(block_name, msg, input).await
    }
    async fn call_block_with_attachments(
        &self,
        block_name: &str,
        msg: Message,
        input: InputStream,
        attachments: BTreeMap<String, Attachment>,
    ) -> OutputStream {
        unsafe { &*self.0 }
            .call_block_with_attachments(block_name, msg, input, attachments)
            .await
    }
    fn lookup_attachment(&self, id: &str) -> Option<Attachment> {
        unsafe { &*self.0 }.lookup_attachment(id)
    }
    fn is_cancelled(&self) -> bool {
        unsafe { &*self.0 }.is_cancelled()
    }
    fn config_get(&self, key: &str) -> Option<&str> {
        unsafe { &*self.0 }.config_get(key)
    }
    fn registered_blocks(&self) -> Vec<crate::block::BlockInfo> {
        unsafe { &*self.0 }.registered_blocks()
    }
    fn flow_infos(&self) -> Vec<wafer_flow::FlowInfo> {
        unsafe { &*self.0 }.flow_infos()
    }
    fn flow_defs(&self) -> Vec<wafer_flow::WaferFlow> {
        unsafe { &*self.0 }.flow_defs()
    }
    fn caller_id(&self) -> Option<&str> {
        unsafe { &*self.0 }.caller_id()
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        // `ContextWrapper` is a thin pointer wrapper around a non-'static
        // `&dyn Context` extended via the `ContextGuard`. Cloning it
        // produces another wrapper aliasing the same pointer. The same
        // lifetime contract as the rest of this module applies: the new
        // Arc must not outlive the originating `ContextGuard`. WASM
        // blocks that need an owning context handle past the lifecycle
        // call must arrange that on the runtime side (the wasmi block
        // dispatcher already drops the guard at end-of-call), not via
        // this method.
        Arc::new(ContextWrapper(self.0))
    }
}
