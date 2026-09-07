//! Explicit static block registration — the path `linkme` cannot serve.
//!
//! `register_static_block!` collects blocks into a `linkme` distributed
//! slice, which is an ELF/Mach-O/PE link section. On `wasm32` there is no
//! such section, so the macro used to expand to nothing at all and a
//! wasm32 embedder got an empty registry: every consumer had to re-list the
//! same block crates by hand, once as `use_static_blocks!` link anchors and
//! again as `register_block(name, Arc::new(Ty::new()))` calls, with nothing
//! keeping the two lists in step.
//!
//! `use_static_blocks!` now emits `WAFER_STATIC_BLOCKS`: the entries from
//! the named crates that link-time collection cannot reach on this target.
//! It is empty wherever `linkme` works, so the call site needs no `cfg` —
//! which is what this file pins on the native side. The wasm32 side is
//! pinned by compiling `crates/wafer-block/tests/wasm_static_blocks`, whose
//! `WAFER_STATIC_BLOCKS` is asserted at compile time to hold one entry per
//! named crate (`scripts/check.sh wasm`).

use std::sync::Arc;

use wafer_block::{
    block::Block,
    context::Context,
    core_types::Message,
    error::RuntimeError,
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
    StaticBlockRegistration,
};
use wafer_block_macro::wafer_async_trait;

// The single list. On this target the linker plus `linkme` do the work and
// `WAFER_STATIC_BLOCKS` comes out empty; on wasm32 the same line is what
// carries cors and router into the runtime.
wafer_block::use_static_blocks!(wafer_block_cors, wafer_block_router);

struct NoopBlock;

#[wafer_async_trait]
impl Block for NoopBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/explicit",
            "1.0.0",
            "http-handler@v1",
            "Explicitly registered",
        )
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::drop_request()
    }
}

static EXPLICIT: StaticBlockRegistration = StaticBlockRegistration {
    name: "test/explicit",
    factory: || Arc::new(NoopBlock) as Arc<dyn Block>,
};

/// Where `linkme` works, the explicit list is empty and the anchors are
/// what registered the blocks — so a consumer can call
/// `register_static_blocks(WAFER_STATIC_BLOCKS)` unconditionally without
/// double-registering anything.
#[test]
fn the_explicit_list_is_empty_where_link_time_collection_works() {
    assert!(
        WAFER_STATIC_BLOCKS.is_empty(),
        "linkme collected these already; a non-empty list here would be a \
         second registration of every named crate's block, got: {:?}",
        WAFER_STATIC_BLOCKS
            .iter()
            .map(|r| r.name)
            .collect::<Vec<_>>()
    );

    let mut wafer = wafer_run::Wafer::builder()
        .disable_lockfile()
        .build()
        .expect("build");
    wafer
        .register_static_blocks(WAFER_STATIC_BLOCKS)
        .expect("registering an empty list is a no-op");

    for name in ["wafer-run/cors", "wafer-run/router"] {
        assert!(
            wafer.has_block(name),
            "the `use_static_blocks!` anchors must still force-link {name}"
        );
    }
}

/// The runtime-side half: an explicitly supplied slice registers exactly
/// like a link-time-collected one.
#[test]
fn register_static_blocks_registers_every_entry() {
    let mut wafer = wafer_run::Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("build");
    assert!(!wafer.has_block("test/explicit"));

    wafer
        .register_static_blocks(&[&EXPLICIT])
        .expect("explicit registration");

    assert!(wafer.has_block("test/explicit"));
}

/// A name registered twice names the offender, the same way
/// `load_inventory_blocks` does — both go through one helper.
#[test]
fn register_static_blocks_names_the_colliding_block() {
    let mut wafer = wafer_run::Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("build");
    wafer
        .register_static_blocks(&[&EXPLICIT])
        .expect("first registration");

    let err = wafer
        .register_static_blocks(&[&EXPLICIT])
        .expect_err("a second registration of the same name must not pass silently");

    assert!(
        matches!(&err, RuntimeError::Inventory { name, .. } if name == "test/explicit"),
        "the error must name the offending block, got: {err:?}"
    );
}
