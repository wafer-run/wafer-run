//! Integration tests for Wave 16 PR B: Wafer::seal() must walk every
//! wafer-run/router block-config's `routes` array (canonical and aliased
//! registrations) and aggregate unresolvable `block` references into
//! RuntimeError::BlocksNotFound alongside flow-step references.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    error::BlockReferenceSource,
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo,
};
// Pull in the router crate so its `register_static_block!` entry stays in
// the linked binary. Without this `use _`, the linker DCEs the crate and
// the `wafer-run/router` entry never reaches `STATIC_BLOCK_REGISTRATIONS`,
// leaving these tests with no router registered.
use wafer_block_router as _;
use wafer_run::{error::RuntimeError, StaticConfigSource, Wafer};

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

struct NoopBlock(&'static str);

#[async_trait]
impl Block for NoopBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.0, "0.0.1", "test/iface@v1", "noop for seal tests")
    }
    async fn lifecycle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        _e: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::respond(b"unreachable".to_vec())
    }
}

fn flow_with_steps(id: &str, steps: Vec<wafer_flow::Step>) -> wafer_flow::WaferFlow {
    wafer_flow::WaferFlow {
        id: id.to_string(),
        name: format!("Test flow: {id}"),
        version: "0.0.1".to_string(),
        description: None,
        input: None,
        output: None,
        steps,
        config: None,
        blocks: None,
        config_map: None,
        config_defaults: None,
    }
}

fn step(id: &str, block: &str) -> wafer_flow::Step {
    wafer_flow::Step {
        id: id.to_string(),
        block: block.to_string(),
        input: None,
        next: None,
        each: None,
        parallel: None,
        description: None,
        config: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `wafer-run/router` is already auto-registered via linkme when this test
/// binary links against `wafer-block-router`. Do NOT call
/// `register_block("wafer-run/router", ...)` — it will return
/// `Err(RuntimeError::DuplicateBlock)`. Just add the block config and seal.
#[tokio::test]
async fn seal_rejects_router_route_referencing_unregistered_block() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.add_block_config(
        "wafer-run/router",
        json!({
            "routes": [
                {"path": "/x", "block": "example/missing", "methods": ["GET"]}
            ]
        }),
    );

    let result = wafer.seal().await;
    match result {
        Err(RuntimeError::BlocksNotFound(errs)) => {
            assert_eq!(
                errs.len(),
                1,
                "expected one missing-block entry, got: {errs:?}"
            );
            assert_eq!(errs[0].name, "example/missing");
            assert_eq!(errs[0].sources.len(), 1);
            match &errs[0].sources[0] {
                BlockReferenceSource::BlockConfig {
                    from_block,
                    location,
                    detail,
                } => {
                    assert_eq!(from_block, "wafer-run/router");
                    assert_eq!(location, "route /x");
                    assert_eq!(detail.as_deref(), Some("GET"));
                }
                other => panic!("expected BlockConfig source, got {other:?}"),
            }
        }
        other => panic!(
            "expected Err(RuntimeError::BlocksNotFound), got {:?}",
            other.as_ref().err()
        ),
    }
}

#[tokio::test]
async fn seal_router_route_walks_aliased_router() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    // `wafer-run/router` is already registered via linkme; just add the alias.
    wafer
        .add_alias("my-router", "wafer-run/router")
        .expect("add_alias");
    wafer.add_block_config(
        "my-router",
        json!({
            "routes": [
                {"path": "/x", "block": "example/missing", "actions": ["create"]}
            ]
        }),
    );

    let result = wafer.seal().await;
    match result {
        Err(RuntimeError::BlocksNotFound(errs)) => {
            assert_eq!(errs.len(), 1, "got: {errs:?}");
            assert_eq!(errs[0].name, "example/missing");
            match &errs[0].sources[0] {
                BlockReferenceSource::BlockConfig { from_block, .. } => {
                    assert_eq!(from_block, "my-router");
                }
                other => panic!("expected BlockConfig source, got {other:?}"),
            }
        }
        other => panic!(
            "expected Err(RuntimeError::BlocksNotFound), got {:?}",
            other.as_ref().err()
        ),
    }
}

#[tokio::test]
async fn seal_router_route_rejects_flow_target() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.add_flow(flow_with_steps(
        "my-flow",
        vec![step("only", "example/present")],
    ));
    wafer
        .register_block("example/present", Arc::new(NoopBlock("example/present")))
        .expect("register noop");
    wafer.add_block_config(
        "wafer-run/router",
        json!({
            "routes": [
                {"path": "/x", "block": "my-flow", "methods": ["GET"]}
            ]
        }),
    );

    // `Context::call_block` (context.rs:251) dispatches via `all_blocks`
    // (blocks + aliases), which does not contain flows — so routing to a
    // flow id would 404 at runtime. seal() must catch this up front, same
    // invariant as the flow-step walk.
    match wafer.seal().await {
        Err(RuntimeError::BlocksNotFound(errs)) => {
            assert_eq!(errs.len(), 1, "expected single missing entry: {errs:?}");
            assert_eq!(errs[0].name, "my-flow");
        }
        other => panic!(
            "expected Err(BlocksNotFound) for flow-targeted route, got {:?}",
            other.as_ref().err()
        ),
    }
}

#[tokio::test]
async fn seal_collapses_flow_and_router_refs_to_same_missing_block() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    // `wafer-run/router` is already registered via linkme.
    wafer.add_flow(flow_with_steps(
        "my-flow",
        vec![step("from-flow", "example/missing")],
    ));
    wafer.add_block_config(
        "wafer-run/router",
        json!({
            "routes": [
                {"path": "/x", "block": "example/missing", "methods": ["POST"]}
            ]
        }),
    );

    let result = wafer.seal().await;
    match result {
        Err(RuntimeError::BlocksNotFound(errs)) => {
            assert_eq!(errs.len(), 1, "expected single missing entry: {errs:?}");
            assert_eq!(errs[0].name, "example/missing");
            assert_eq!(
                errs[0].sources.len(),
                2,
                "expected two sources (flow + router), got: {:?}",
                errs[0].sources
            );
            let has_flow = errs[0]
                .sources
                .iter()
                .any(|s| matches!(s, BlockReferenceSource::Flow { .. }));
            let has_router = errs[0]
                .sources
                .iter()
                .any(|s| matches!(s, BlockReferenceSource::BlockConfig { .. }));
            assert!(has_flow, "missing Flow source: {:?}", errs[0].sources);
            assert!(
                has_router,
                "missing BlockConfig source: {:?}",
                errs[0].sources
            );
        }
        other => panic!(
            "expected Err(RuntimeError::BlocksNotFound), got {:?}",
            other.as_ref().err()
        ),
    }
}
