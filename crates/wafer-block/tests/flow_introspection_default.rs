//! Verifies the new `Context::flow_introspection` default returns None,
//! and that the FlowIntrospection trait is object-safe.

use std::sync::Arc;

use wafer_block::introspection::FlowIntrospection;
use wafer_block::{streams::output::OutputStream, Context, InputStream, Message};
use wafer_block_macro::wafer_async_trait;

#[derive(Clone)]
struct MinimalContext;

#[wafer_async_trait]
impl Context for MinimalContext {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::respond(b"ok".to_vec())
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

#[test]
fn default_flow_introspection_returns_none() {
    let ctx = MinimalContext;
    assert!(ctx.flow_introspection().is_none());
}

#[test]
fn flow_introspection_is_object_safe() {
    fn _take_dyn(_: &dyn FlowIntrospection) {}
}
