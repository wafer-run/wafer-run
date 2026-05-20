//! Default Context impls for the attachment APIs: no-op semantics so that
//! existing native code paths (tests, native runtime) compile and run unchanged.

use std::{collections::BTreeMap, sync::Arc};
use wafer_block_macro::wafer_async_trait;

use wafer_block::{streams::output::OutputStream, Attachment, Context, InputStream, Message};

#[derive(Clone)]
struct PlainContext;

#[wafer_async_trait]
impl Context for PlainContext {
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

#[tokio::test]
async fn default_call_block_with_attachments_drops_attachments_and_calls_through() {
    let ctx = PlainContext;
    let mut atts = BTreeMap::new();
    atts.insert(
        "ignored".into(),
        Attachment {
            mime: "x".into(),
            bytes: vec![1, 2, 3],
            filename: None,
        },
    );
    let out = ctx
        .call_block_with_attachments("any/block", Message::new("k"), InputStream::empty(), atts)
        .await;
    let buf = out.collect_buffered().await.expect("complete");
    assert_eq!(buf.body, b"ok");
}

#[test]
fn default_lookup_attachment_returns_none() {
    let ctx = PlainContext;
    assert!(ctx.lookup_attachment("anything").is_none());
}

#[tokio::test]
async fn clone_arc_yields_owned_handle_callable_through_dyn() {
    // Construct a Context, take a `&dyn Context` borrow, then upgrade it to
    // an owning `Arc<dyn Context>`. The clone must be usable independently
    // of the original — exercise it through a trait method.
    let arc: Arc<dyn Context> = {
        let ctx = PlainContext;
        let dyn_ref: &dyn Context = &ctx;
        dyn_ref.clone_arc()
        // `ctx` goes out of scope here; the Arc must still be usable below.
    };

    let out = arc
        .call_block_buffered("any/block", Message::new("k"), b"")
        .await
        .expect("buffered ok");
    assert_eq!(out.body, b"ok");
}
