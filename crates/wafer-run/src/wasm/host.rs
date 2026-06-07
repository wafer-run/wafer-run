use std::{collections::BTreeMap, sync::Arc};

use wafer_block::{
    core_types::Attachment,
    streams::{input::InputStream, output::OutputStream},
    Message,
};
use wafer_block_macro::wafer_async_trait;

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
        // Runtime assertion (not `debug_assert`) — the underlying
        // `ContextWrapper` extends a non-`'static` `&dyn Context` via
        // `transmute`. If any cloned `Arc` outlives the guard it dereferences
        // freed memory. Cheap counter check; failing closed in release builds
        // is the right call.
        assert_eq!(
            Arc::strong_count(&self.wrapper),
            1,
            "BUG: ContextGuard dropped while cloned Arcs still exist — use-after-free hazard",
        );
    }
}

struct ContextWrapper(*const dyn Context);
// SAFETY: The pointed-to `dyn Context` is the runtime's real context, which is
// itself `Send + Sync` (every `Context` impl crossing the wasmi host boundary
// satisfies those bounds). `ContextGuard::new` only transmutes the borrow's
// lifetime to `'static` — it extends *how long* the reference is held, never
// the thread bounds of the pointee — so asserting `Send + Sync` on the wrapper
// does not relax any guarantee the original `&dyn Context` already provided.
// The pointer's validity window is bounded by `ContextGuard`: the guard owns
// the only `Arc<ContextWrapper>`, hands out clones via `as_arc`, and its `Drop`
// asserts `Arc::strong_count == 1`, so no clone can outlive the borrowed
// context and dereference freed memory. (`clone_arc` panics for the same
// reason — it would mint an Arc outside the guard's strong-count accounting.)
unsafe impl Send for ContextWrapper {}
unsafe impl Sync for ContextWrapper {}

#[wafer_async_trait]
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
    fn flow_introspection(&self) -> Option<&dyn wafer_block::introspection::FlowIntrospection> {
        unsafe { &*self.0 }.flow_introspection()
    }
    fn caller_id(&self) -> Option<&str> {
        unsafe { &*self.0 }.caller_id()
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        // `ContextWrapper` extends a non-'static `&dyn Context` via
        // `transmute` and only stays valid for the lifetime of the
        // owning `ContextGuard`. A clone produced here would alias the
        // raw pointer without participating in `ContextGuard`'s strong-
        // count assertion, so it could outlive the borrowed context
        // and dereference freed memory.
        //
        // No host code in this crate calls `clone_arc` on the wasmi
        // ContextWrapper path. We panic rather than silently hand back
        // an Arc that bypasses the guard invariant — easier to catch
        // misuse than to chase a use-after-free.
        panic!(
            "Context::clone_arc called on wasmi ContextWrapper — \
             the underlying context is borrowed and cannot be cloned \
             into an owning Arc",
        );
    }
}
