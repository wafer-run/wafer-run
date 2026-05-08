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
}

/// Construct a no-op [`Context`] suitable for tests that don't exercise
/// `call_block` / config / cancellation. Returns a boxed trait object so call
/// sites don't need to name the underlying type.
pub fn noop_context() -> Box<dyn Context> {
    Box::new(NoopContext)
}
