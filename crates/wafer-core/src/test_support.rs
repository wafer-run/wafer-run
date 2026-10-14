//! Test-only helpers shared across `wafer-core` unit tests.
//!
//! This module is gated behind `#[cfg(test)]` and is not part of the public
//! crate surface. Helpers here exist to keep individual `mod tests` blocks
//! small and focused on the behavior under test.

use wafer_block::{
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    Message,
};

/// Minimal `Context` impl that panics on every method that takes meaningful
/// runtime state. Use this when a test exercises a Block path that should not
/// reach back into the runtime (e.g. lifecycle hooks that only invoke service
/// methods). Any unexpected runtime call surfaces as a loud panic rather than
/// silently returning a stub value.
#[derive(Clone)]
struct NoopContext;

#[async_trait::async_trait]
impl Context for NoopContext {
    async fn call_block(
        &self,
        block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        panic!("NoopContext::call_block called unexpectedly: block_name={block_name}");
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        std::sync::Arc::new(self.clone())
    }
    // Denies every access, as the trait's default `check_resource_access` does.
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: wafer_block::types::ResourceType,
        _access: wafer_block::types::ResourceAccess,
    ) -> bool {
        false
    }
}

/// Construct a no-op [`Context`] suitable for tests that don't exercise
/// `call_block` / config / cancellation. Returns a boxed trait object so call
/// sites don't need to name the underlying type.
pub fn noop_context() -> Box<dyn Context> {
    Box::new(NoopContext)
}

/// A [`Context`] that admits every resource access and answers nothing else.
/// For handler tests whose subject is what happens after authorization.
#[derive(Clone)]
struct AdmitAllContext;

#[async_trait::async_trait]
impl Context for AdmitAllContext {
    async fn call_block(
        &self,
        block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        panic!("AdmitAllContext::call_block called unexpectedly: block_name={block_name}");
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        std::sync::Arc::new(self.clone())
    }

    fn check_resource_access(
        &self,
        _resource: &str,
        _resource_type: wafer_block::types::ResourceType,
        _access: wafer_block::types::ResourceAccess,
    ) -> Result<(), wafer_block::WaferError> {
        Ok(())
    }

    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: wafer_block::types::ResourceType,
        _access: wafer_block::types::ResourceAccess,
    ) -> bool {
        true
    }
}

/// Construct a [`Context`] that admits every resource access.
pub fn admit_all_context() -> Box<dyn Context> {
    Box::new(AdmitAllContext)
}

/// A service stream that never yields and ignores its cancellation token —
/// the worst case a relay must still let go of — and reports when it is
/// dropped.
pub struct Silent<T> {
    dropped: Option<futures::channel::oneshot::Sender<()>>,
    _item: std::marker::PhantomData<fn() -> T>,
}

impl<T> Silent<T> {
    /// The stream, and the receiver its drop resolves.
    pub fn new() -> (Self, futures::channel::oneshot::Receiver<()>) {
        let (tx, rx) = futures::channel::oneshot::channel();
        (
            Self {
                dropped: Some(tx),
                _item: std::marker::PhantomData,
            },
            rx,
        )
    }
}

impl<T> futures::Stream for Silent<T> {
    type Item = T;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<T>> {
        std::task::Poll::Pending
    }
}

impl<T> Drop for Silent<T> {
    fn drop(&mut self) {
        if let Some(dropped) = self.dropped.take() {
            let _ = dropped.send(());
        }
    }
}
