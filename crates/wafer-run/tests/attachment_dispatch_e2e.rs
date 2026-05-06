//! Native-runtime carry-forward test — caller block attaches an attachment via
//! `Context::call_block_buffered_with_attachments`; callee block reads it via
//! `Context::lookup_attachment`. No wasm involved — proves the runtime-side
//! plumbing wires the slot through.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use wafer_block::{
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
    Attachment, Block, Context, Message,
};
use wafer_run::Wafer;

#[derive(Clone, Default)]
struct CalleeBlock {
    seen: Arc<Mutex<Option<Attachment>>>,
}

#[async_trait]
impl Block for CalleeBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/callee", "0.0.0", "h@v1", "")
    }
    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let att = ctx.lookup_attachment("call_1");
        *self.seen.lock().unwrap() = att;
        OutputStream::respond(b"ok".to_vec())
    }
}

#[derive(Default)]
struct CallerBlock;

#[async_trait]
impl Block for CallerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/caller", "0.0.0", "h@v1", "")
    }
    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let mut atts = BTreeMap::new();
        atts.insert(
            "call_1".to_string(),
            Attachment {
                mime: "image/png".into(),
                bytes: vec![9, 9, 9],
                filename: None,
            },
        );
        let _ = ctx
            .call_block_buffered_with_attachments("test/callee", Message::new("k"), &[], atts)
            .await;
        OutputStream::respond(b"ok".to_vec())
    }
}

#[tokio::test]
async fn native_runtime_carries_attachment_to_callee() {
    let callee_seen = Arc::new(Mutex::new(None));
    let callee = Arc::new(CalleeBlock {
        seen: callee_seen.clone(),
    });
    let caller = Arc::new(CallerBlock);

    // Build a Wafer with no auto-registration; register the two test blocks
    // directly via the BlockRegistry trait, then drive the caller via
    // `run_block` (the same top-level dispatch path used by other tests).
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");

    wafer
        .register_block("test/callee", callee)
        .expect("register callee");
    wafer
        .register_block("test/caller", caller)
        .expect("register caller");

    let wafer = wafer.start().await.expect("start runtime");

    let _ = wafer
        .run_block("test/caller", Message::new("k"), InputStream::empty())
        .await
        .collect_buffered()
        .await
        .expect("caller dispatch");

    let seen = callee_seen.lock().unwrap().clone();
    assert!(seen.is_some(), "callee saw no attachment");
    let att = seen.unwrap();
    assert_eq!(att.bytes, vec![9, 9, 9]);
    assert_eq!(att.mime, "image/png");

    // Negative-path coverage: a plain call_block (no attachments) to
    // test/callee should leave lookup_attachment returning None.
    *callee_seen.lock().unwrap() = None; // reset slot
    let _ = wafer
        .run_block("test/callee", Message::new("k"), InputStream::empty())
        .await
        .collect_buffered()
        .await
        .expect("direct callee dispatch");

    let seen_plain = callee_seen.lock().unwrap().clone();
    assert!(
        seen_plain.is_none(),
        "lookup_attachment should return None on plain call_block (no attachments)"
    );
}
