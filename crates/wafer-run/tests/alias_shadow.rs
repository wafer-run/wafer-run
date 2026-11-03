//! A name is a block or an alias, never both.
//!
//! An alias that shadowed a registered block — in either order of
//! registration — left the block under the name while dispatch resolved the
//! name to the alias's target. With that target unregistered, the lookup
//! fell back to the shadowed block under the target's identity, and
//! `run_block` / `call_block` on the name panicked on the target's missing
//! init slot. Both orders are now refused where the second name is added.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use wafer_block::{
    context::Context,
    core_types::Message,
    error::{AliasError, RuntimeError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo,
};
use wafer_run::Wafer;

/// Counts its `info()` reads and its dispatches.
struct Named {
    name: &'static str,
    info_calls: Arc<AtomicUsize>,
    handle_calls: Arc<AtomicUsize>,
}

impl Named {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            info_calls: Arc::default(),
            handle_calls: Arc::default(),
        }
    }
}

#[async_trait]
impl Block for Named {
    fn info(&self) -> BlockInfo {
        self.info_calls.fetch_add(1, Ordering::SeqCst);
        BlockInfo::new(self.name, "0.1.0", "test/named@v1", "a named block")
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        self.handle_calls.fetch_add(1, Ordering::SeqCst);
        OutputStream::respond(self.name.as_bytes().to_vec())
    }
}

/// Calls `target` and answers what it answered.
struct Caller(&'static str);

#[async_trait]
impl Block for Caller {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/caller", "0.1.0", "test/caller@v1", "calls a block")
    }

    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        ctx.call_block(self.0, Message::new("call"), InputStream::empty())
            .await
    }
}

fn empty_wafer() -> Wafer {
    Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible")
}

async fn body(out: OutputStream) -> Vec<u8> {
    out.collect_buffered()
        .await
        .unwrap_or_else(|e| panic!("dispatch failed: {e:?}"))
        .body
}

/// Block first, then the alias: `add_alias` refuses it, and the block keeps
/// its name for every dispatch path.
#[tokio::test]
async fn an_alias_named_like_a_registered_block_is_refused() {
    let block = Arc::new(Named::new("a/x"));
    let handles = block.handle_calls.clone();
    let mut wafer = empty_wafer();
    wafer.register_block("a/x", block).expect("register a/x");
    wafer
        .register_block("test/caller", Arc::new(Caller("a/x")))
        .expect("register the caller");

    match wafer.add_alias("a/x", "a/missing") {
        Err(AliasError::ShadowsBlock { alias }) => assert_eq!(alias, "a/x"),
        other => panic!("expected ShadowsBlock, got {other:?}"),
    }
    assert_eq!(wafer.canonicalize("a/x"), "a/x", "no alias was recorded");

    wafer.seal().await.expect("seal");
    let direct = wafer
        .run_block("a/x", Message::new("run"), InputStream::empty())
        .await;
    assert_eq!(body(direct).await, b"a/x");
    let called = wafer
        .run_block("test/caller", Message::new("run"), InputStream::empty())
        .await;
    assert_eq!(body(called).await, b"a/x");
    assert_eq!(handles.load(Ordering::SeqCst), 2);
}

/// The alias first, then the block: registration refuses the name before it
/// reads the block's declaration.
#[tokio::test]
async fn a_block_named_like_an_alias_is_refused() {
    let block = Arc::new(Named::new("a/x"));
    let info_calls = block.info_calls.clone();
    let mut wafer = empty_wafer();
    wafer.add_alias("a/x", "a/y").expect("alias a/x to a/y");

    match wafer.register_block("a/x", block) {
        Err(RuntimeError::NameIsAlias { name, target }) => {
            assert_eq!(name, "a/x");
            assert_eq!(target, "a/y");
        }
        other => panic!("expected NameIsAlias, got {other:?}"),
    }
    assert_eq!(info_calls.load(Ordering::SeqCst), 0, "refused unread");
    assert!(!wafer.has_block("a/x"));
}

/// An alias of a registered block still dispatches to it, by `run_block`
/// and by `call_block`.
#[tokio::test]
async fn an_alias_of_a_registered_block_dispatches_to_it() {
    let block = Arc::new(Named::new("a/y"));
    let handles = block.handle_calls.clone();
    let mut wafer = empty_wafer();
    wafer.register_block("a/y", block).expect("register a/y");
    wafer
        .register_block("test/caller", Arc::new(Caller("a/x")))
        .expect("register the caller");
    wafer.add_alias("a/x", "a/y").expect("alias a/x to a/y");
    wafer.seal().await.expect("seal");

    let direct = wafer
        .run_block("a/x", Message::new("run"), InputStream::empty())
        .await;
    assert_eq!(body(direct).await, b"a/y");
    let called = wafer
        .run_block("test/caller", Message::new("run"), InputStream::empty())
        .await;
    assert_eq!(body(called).await, b"a/y");
    assert_eq!(handles.load(Ordering::SeqCst), 2);
}
