//! Default Context impls for the attachment APIs: no-op semantics so that
//! existing native code paths (tests, native runtime) compile and run unchanged.

use std::collections::BTreeMap;

use wafer_block::{streams::output::OutputStream, Attachment, Context, InputStream, Message};

struct PlainContext;

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
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
