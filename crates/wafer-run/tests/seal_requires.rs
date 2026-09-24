//! `seal()` refuses to boot while a registered block's `requires` names a
//! block that is not registered, and the runtime answers dispatch to a block
//! it does not have with `Unimplemented`, never `NotFound`.
//!
//! `NotFound` is how a service says the thing a request names does not
//! exist, and clients act on it (`config::get_default` returns its default,
//! `database::upsert_by_field` creates the row). A runtime `NotFound` for an
//! absent service block would read as that answer: the client falls back
//! as if the service had been asked. A block that cannot run without a
//! service says so in `requires` and the runtime refuses to start without
//! it; a block that can run without one lists it in `optional_requires`.

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    error::BlockReferenceSource,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    Block, BlockInfo, ErrorCode,
};
use wafer_run::{Context, RuntimeError, Wafer};

/// A block with the given dependency lists. Handling a message calls the
/// block named by the request's `target` meta and answers with the reply's
/// body or error.
struct Dependent {
    name: &'static str,
    requires: Vec<&'static str>,
    optional_requires: Vec<&'static str>,
}

impl Dependent {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            requires: Vec::new(),
            optional_requires: Vec::new(),
        }
    }
    fn requires(mut self, names: &[&'static str]) -> Self {
        self.requires = names.to_vec();
        self
    }
    fn optional_requires(mut self, names: &[&'static str]) -> Self {
        self.optional_requires = names.to_vec();
        self
    }
}

#[async_trait]
impl Block for Dependent {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "dependent")
            .requires(self.requires.iter().map(ToString::to_string).collect())
            .optional_requires(
                self.optional_requires
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            )
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let target = msg.get_meta("target").to_string();
        ctx.call_block(&target, Message::new("ping"), InputStream::empty())
            .await
    }
}

fn wafer() -> Wafer {
    Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build")
}

/// The error `run_block(block, …)` ends with, or `None` if it answered.
async fn call_error(wafer: &Wafer, block: &str, target: &str) -> Option<WaferError> {
    let mut msg = Message::new("go");
    msg.set_meta("target", target);
    match wafer
        .run_block(block, msg, InputStream::empty())
        .await
        .collect_buffered()
        .await
    {
        Ok(_) => None,
        Err(TerminalNotResponse::Error(e)) => Some(e),
        Err(other) => panic!("neither a response nor an error: {other:?}"),
    }
}

#[tokio::test]
async fn seal_refuses_a_block_whose_requires_names_an_unregistered_block() {
    let mut wafer = wafer();
    wafer
        .register_block(
            "acme/needs-config",
            Arc::new(Dependent::new("acme/needs-config").requires(&["wafer-run/config"])),
        )
        .expect("register");

    match wafer.seal().await {
        Err(RuntimeError::BlocksNotFound(missing)) => {
            assert_eq!(missing.len(), 1, "{missing:?}");
            assert_eq!(missing[0].name, "wafer-run/config");
            assert!(
                matches!(
                    missing[0].sources.as_slice(),
                    [BlockReferenceSource::Requires { from_block }] if from_block == "acme/needs-config"
                ),
                "{missing:?}"
            );
        }
        other => panic!("expected BlocksNotFound, got {other:?}"),
    }
}

/// Every requiring block is named, once per missing block, in name order.
#[tokio::test]
async fn seal_names_every_block_that_requires_a_missing_block() {
    let mut wafer = wafer();
    for name in ["acme/b", "acme/a"] {
        wafer
            .register_block(
                name,
                Arc::new(Dependent::new(name).requires(&["acme/zed", "acme/dep"])),
            )
            .expect("register");
    }

    let Err(RuntimeError::BlocksNotFound(missing)) = wafer.seal().await else {
        panic!("expected BlocksNotFound");
    };
    let rendered: Vec<(String, Vec<String>)> = missing
        .iter()
        .map(|e| {
            let from = e
                .sources
                .iter()
                .map(|s| match s {
                    BlockReferenceSource::Requires { from_block } => from_block.clone(),
                    other => panic!("unexpected source {other:?}"),
                })
                .collect();
            (e.name.clone(), from)
        })
        .collect();
    assert_eq!(
        rendered,
        vec![
            (
                "acme/dep".to_string(),
                vec!["acme/a".into(), "acme/b".into()]
            ),
            (
                "acme/zed".to_string(),
                vec!["acme/a".into(), "acme/b".into()]
            ),
        ]
    );
}

#[tokio::test]
async fn seal_resolves_a_requires_entry_through_an_alias() {
    let mut wafer = wafer();
    wafer
        .register_block("acme/store", Arc::new(Dependent::new("acme/store")))
        .expect("register");
    wafer.add_alias("store", "acme/store").expect("alias");
    wafer
        .register_block(
            "acme/user",
            Arc::new(Dependent::new("acme/user").requires(&["store"])),
        )
        .expect("register");

    wafer
        .seal()
        .await
        .expect("an alias of a registered block meets requires");
}

/// An `optional_requires` entry does not hold up boot. It is still on the
/// block's allowlist, so the call reaches dispatch and fails there with
/// `Unimplemented`; a block outside both lists stays denied.
#[tokio::test]
async fn optional_requires_boots_without_the_block_and_calling_it_is_unimplemented() {
    let mut wafer = wafer();
    wafer
        .register_block(
            "acme/vector",
            Arc::new(Dependent::new("acme/vector").optional_requires(&["acme/embedder"])),
        )
        .expect("register");
    wafer
        .register_block("acme/other", Arc::new(Dependent::new("acme/other")))
        .expect("register");
    wafer
        .seal()
        .await
        .expect("an absent optional dependency is not a boot error");

    let absent = call_error(&wafer, "acme/vector", "acme/embedder")
        .await
        .expect("the call to an absent block fails");
    assert_eq!(absent.code, ErrorCode::Unimplemented, "{absent:?}");

    let denied = call_error(&wafer, "acme/vector", "acme/other")
        .await
        .expect("a block outside the allowlist is denied");
    assert_eq!(denied.code, ErrorCode::PermissionDenied, "{denied:?}");
}

/// A block with no dependency lists may call anything; one that is not
/// registered is the runtime's `Unimplemented`, never `NotFound`.
#[tokio::test]
async fn call_block_to_an_unregistered_block_is_unimplemented() {
    let mut wafer = wafer();
    wafer
        .register_block("acme/caller", Arc::new(Dependent::new("acme/caller")))
        .expect("register");
    wafer.seal().await.expect("seal");

    let e = call_error(&wafer, "acme/caller", "wafer-run/database")
        .await
        .expect("the call fails");
    assert_eq!(e.code, ErrorCode::Unimplemented, "{e:?}");
}

#[tokio::test]
async fn run_block_of_an_unregistered_block_is_unimplemented() {
    let mut wafer = wafer();
    wafer.seal().await.expect("seal");

    let e = call_error(&wafer, "acme/nowhere", "unused")
        .await
        .expect("dispatch fails");
    assert_eq!(e.code, ErrorCode::Unimplemented, "{e:?}");
}
