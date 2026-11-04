//! A block's `BlockInfo` is read exactly once per registration.
//!
//! `Block::info()` builds its `BlockInfo` from scratch on every call, and a
//! large one is expensive: an embedder's 30-block runtime spent about 60 % of
//! a cold build re-reading them, a 605 KB declaration among them. The
//! runtime reads each block's `info()` once, when the block is registered,
//! and every later pass reads that copy. A block here counts its `info()`
//! calls while it is put through everything a runtime does with a block:
//! registration, the admin-block grant rescan, `seal()`, `block_infos()`,
//! config validation (on the `Wafer` and from a context), `init_block`,
//! top-level dispatch (by name and by alias), a flow step, `call_block` from
//! another block (by name and by alias), the Start and Stop lifecycles.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use wafer_block::{
    context::Context,
    core_types::{LifecycleEvent, Message, WaferError},
    error::RuntimeError,
    streams::{input::InputStream, output::OutputStream},
    types::ResourceGrant,
    AuthLevel, Block, BlockEndpoint, BlockInfo, ConfigVar,
};
use wafer_run::Wafer;

const COUNTED: &str = "test/counted";
const ALIAS: &str = "test/counted-alias";
const CALLER: &str = "test/caller";
const HELPER: &str = "test/helper";

/// Counts its `info()` calls and its `handle` calls. Its declaration
/// exercises every pass that reads one: grants (registration and the
/// admin-block rescan), `requires` (seal's unmet-requires check and every
/// frame's `call_block` allowlist), capabilities (seal), an agent tool and a
/// route (seal's two uniqueness checks), a config key (Init and config
/// validation) and an init budget (the Init pipeline).
struct Counted {
    info_calls: Arc<AtomicUsize>,
    handle_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Block for Counted {
    fn info(&self) -> BlockInfo {
        self.info_calls.fetch_add(1, Ordering::SeqCst);
        BlockInfo::new(COUNTED, "0.1.0", "test/counted@v1", "counts info() calls")
            .requires(vec![HELPER.to_string()])
            .config_keys(vec![ConfigVar::new(
                "TEST__COUNTED__GREETING",
                "a key Init and validation load",
                "hello",
            )])
            .grants(vec![ResourceGrant::read(CALLER, "test__counted__things")])
            .endpoints(vec![BlockEndpoint::get("/b/counted/things")
                .auth(AuthLevel::Public)
                .agent_tool("list_counted_things", "List the counted things.")])
            .capabilities(wafer_block::BlockCapabilities::unrestricted())
            .init_timeout(std::time::Duration::from_secs(30))
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        self.handle_calls.fetch_add(1, Ordering::SeqCst);
        OutputStream::respond(b"counted".to_vec())
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(())
    }
}

/// Calls the counted block by name and by alias, and reads the registry
/// views a context offers, from inside a dispatch.
struct Caller;

#[async_trait]
impl Block for Caller {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(CALLER, "0.1.0", "test/caller@v1", "calls the counted block")
            .requires(vec![COUNTED.to_string(), ALIAS.to_string()])
    }

    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let report = ctx.validate_all_block_configs().await;
        assert!(
            report.ok.iter().any(|name| name == COUNTED),
            "the context validated the counted block's config: {report:?}"
        );
        assert!(
            ctx.registered_blocks().iter().any(|b| b.name == COUNTED),
            "the context lists the counted block"
        );
        for target in [COUNTED, ALIAS] {
            let out = ctx
                .call_block(target, Message::new("count"), InputStream::empty())
                .await
                .collect_buffered()
                .await;
            if let Err(e) = out {
                panic!("call_block({target}) failed: {e:?}");
            }
        }
        OutputStream::respond(b"called".to_vec())
    }
}

/// The block the counted one requires, so `seal()` accepts it.
struct Helper;

#[async_trait]
impl Block for Helper {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            HELPER,
            "0.1.0",
            "test/helper@v1",
            "required by the counted block",
        )
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

fn empty_wafer() -> Wafer {
    Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible")
}

fn counted(info_calls: &Arc<AtomicUsize>, handle_calls: &Arc<AtomicUsize>) -> Arc<dyn Block> {
    Arc::new(Counted {
        info_calls: info_calls.clone(),
        handle_calls: handle_calls.clone(),
    })
}

fn single_step_flow(id: &str, block: &str) -> wafer_flow::WaferFlow {
    wafer_flow::WaferFlow {
        id: id.to_string(),
        name: format!("flow {id}"),
        version: "0.0.1".to_string(),
        description: None,
        input: None,
        output: None,
        steps: vec![wafer_flow::Step {
            id: "root".to_string(),
            block: block.to_string(),
            input: None,
            next: None,
            each: None,
            parallel: None,
            description: None,
            config: None,
        }],
        config: None,
        blocks: None,
        config_map: None,
        config_defaults: None,
    }
}

async fn ok(out: OutputStream, what: &str) {
    out.collect_buffered()
        .await
        .unwrap_or_else(|e| panic!("{what} failed: {e:?}"));
}

/// Everything a runtime does with a registered block reads its `BlockInfo`
/// from the registration: `info()` runs once, at `register_block`.
#[tokio::test]
async fn info_is_called_once_across_registration_seal_build_and_dispatch() {
    let info_calls = Arc::new(AtomicUsize::new(0));
    let handle_calls = Arc::new(AtomicUsize::new(0));
    let mut wafer = empty_wafer();

    wafer
        .register_block(COUNTED, counted(&info_calls, &handle_calls))
        .expect("register the counted block");
    assert_eq!(
        info_calls.load(Ordering::SeqCst),
        1,
        "registration reads the declaration"
    );

    wafer
        .register_block(CALLER, Arc::new(Caller))
        .expect("register the caller");
    wafer
        .register_block(HELPER, Arc::new(Helper))
        .expect("register the helper");
    wafer
        .add_alias(ALIAS, COUNTED)
        .expect("alias the counted block");
    // Rescans every registered block's grants.
    wafer.set_admin_block("test/admin");
    wafer
        .add_flow(single_step_flow("count-flow", COUNTED))
        .expect("add the flow");

    wafer.seal().await.expect("seal");

    let infos = wafer.block_infos();
    let info = infos
        .iter()
        .find(|b| b.name == COUNTED)
        .expect("block_infos lists the counted block");
    assert_eq!(info.endpoints.len(), 1, "the registration-time declaration");
    let report = wafer.validate_all_block_configs().await;
    assert!(report.ok.iter().any(|name| name == COUNTED), "{report:?}");

    wafer
        .init_block(COUNTED)
        .await
        .expect("init the counted block");
    for target in [COUNTED, ALIAS] {
        ok(
            wafer
                .run_block(target, Message::new("count"), InputStream::empty())
                .await,
            target,
        )
        .await;
    }
    ok(
        wafer
            .run("count-flow", Message::new("count"), InputStream::empty())
            .await,
        "the flow",
    )
    .await;
    ok(
        wafer
            .run_block(CALLER, Message::new("call"), InputStream::empty())
            .await,
        "the caller",
    )
    .await;
    wafer.init_all_blocks().await;
    wafer.run_start_lifecycle().await;
    wafer.shutdown().await;

    assert_eq!(
        handle_calls.load(Ordering::SeqCst),
        5,
        "every dispatch path reached the counted block: run_block by name and \
         by alias, the flow step, call_block by name and by alias"
    );
    assert_eq!(
        info_calls.load(Ordering::SeqCst),
        1,
        "every pass after registration reads the registered BlockInfo"
    );
}

/// The cache belongs to the registration, not the block type: a new
/// registration (a new runtime, as a rebuilt or reloaded block reaches one)
/// reads the declaration again, and a refused duplicate registration does
/// not read it at all.
#[tokio::test]
async fn each_registration_reads_the_declaration_once() {
    let info_calls = Arc::new(AtomicUsize::new(0));
    let handle_calls = Arc::new(AtomicUsize::new(0));

    let mut first = empty_wafer();
    first
        .register_block(COUNTED, counted(&info_calls, &handle_calls))
        .expect("first registration");
    assert!(matches!(
        first.register_block(COUNTED, counted(&info_calls, &handle_calls)),
        Err(RuntimeError::DuplicateBlock { .. })
    ));
    assert_eq!(
        info_calls.load(Ordering::SeqCst),
        1,
        "a refused duplicate reads nothing"
    );

    let mut second = empty_wafer();
    second
        .register_block(COUNTED, counted(&info_calls, &handle_calls))
        .expect("second runtime's registration");
    second
        .register_block(HELPER, Arc::new(Helper))
        .expect("register the helper");
    second.seal().await.expect("seal");
    second.init_block(COUNTED).await.expect("init");
    assert_eq!(
        info_calls.load(Ordering::SeqCst),
        2,
        "the second registration read its own declaration, once"
    );
}

/// Hands the declarations its context lends to the test.
struct Lender(Arc<std::sync::Mutex<Vec<Arc<BlockInfo>>>>);

#[async_trait]
impl Block for Lender {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/lender",
            "0.1.0",
            "test/lender@v1",
            "lends its context's blocks",
        )
    }

    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        *self.0.lock().expect("lent lock") = ctx.registered_blocks().to_vec();
        OutputStream::respond(Vec::new())
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(())
    }
}

/// The sealed snapshot a context lends is the registrations' own
/// declarations, not copies of them: every entry of
/// `Context::registered_blocks()` is the same allocation as the matching
/// `Wafer::block_infos()` entry.
#[tokio::test]
async fn the_sealed_snapshot_shares_the_registered_declarations() {
    let lent = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut wafer = empty_wafer();
    wafer
        .register_block("test/lender", Arc::new(Lender(Arc::clone(&lent))))
        .expect("register the lender");
    wafer
        .register_block(HELPER, Arc::new(Helper))
        .expect("register a second block");
    wafer.seal().await.expect("seal");
    if let Err(e) = wafer
        .run_block("test/lender", Message::new("lend"), InputStream::empty())
        .await
        .collect_buffered()
        .await
    {
        panic!("run_block(test/lender) failed: {e:?}");
    }

    let lent = lent.lock().expect("lent lock");
    let registered = wafer.block_infos();
    assert_eq!(lent.len(), registered.len());
    for (lent, registered) in lent.iter().zip(&registered) {
        assert_eq!(lent.name, registered.name);
        assert!(
            Arc::ptr_eq(lent, registered),
            "{} is lent as a copy of its registered declaration",
            lent.name
        );
    }
}
