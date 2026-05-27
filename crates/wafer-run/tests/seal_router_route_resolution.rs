//! Integration tests for Wave 16 PR B: Wafer::seal() must walk every
//! wafer-run/router block-config's `routes` array (canonical and aliased
//! registrations) and aggregate unresolvable `block` references into
//! RuntimeError::BlocksNotFound alongside flow-step references.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use wafer_block::{
    core_types::{LifecycleEvent, LifecycleType, Message, WaferError},
    error::BlockReferenceSource,
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo,
};
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
                BlockReferenceSource::RouterRoute {
                    router_block,
                    path,
                    actions,
                } => {
                    assert_eq!(router_block, "wafer-run/router");
                    assert_eq!(path, "/x");
                    assert_eq!(actions, &vec!["GET".to_string()]);
                }
                other => panic!("expected RouterRoute source, got {other:?}"),
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
    wafer.add_alias("my-router", "wafer-run/router");
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
                BlockReferenceSource::RouterRoute { router_block, .. } => {
                    assert_eq!(router_block, "my-router");
                }
                other => panic!("expected RouterRoute source, got {other:?}"),
            }
        }
        other => panic!(
            "expected Err(RuntimeError::BlocksNotFound), got {:?}",
            other.as_ref().err()
        ),
    }
}

#[tokio::test]
async fn seal_router_route_accepts_flow_target() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    // `wafer-run/router` is already registered via linkme.
    // The route targets a flow — seal() must accept flows as valid targets.
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

    let result = wafer.seal().await;
    assert!(
        result.is_ok(),
        "seal() should accept router routes targeting flows, got Err: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn seal_router_route_contract_match_with_block_parser() {
    // Pins the duplicated `parse_routes_for_validation` shape against
    // `wafer-block-router::parse_routes`. Action normalization differs
    // intentionally (we keep raw strings; router normalizes for matching).
    // Compare only (path, block) pairs.
    let cfg = json!({
        "routes": [
            {"path": "/a", "block": "a-block", "actions": ["retrieve"]},
            {"path": "/b", "block": "b-block", "methods": ["GET"]},
            {"path": "/c", "block": "c-block"}
        ]
    });

    // wafer-run side: parse_routes_for_validation takes a serde_json::Value
    let validation_routes = wafer_run::runtime::router_walk::parse_routes_for_validation(&cfg);
    let our_pairs: Vec<(String, String)> = validation_routes
        .iter()
        .map(|r| (r.path.clone(), r.block.clone()))
        .collect();

    // wafer-block-router side: parse_routes takes a BlockConfig.
    // Construct BlockConfig via a fake LifecycleEvent (the only public constructor).
    let event = LifecycleEvent {
        event_type: LifecycleType::Init,
        data: serde_json::to_vec(&cfg).expect("serialize cfg"),
    };
    let block_config = wafer_block::BlockConfig::from_event(&event);
    let router_routes = wafer_block_router::parse_routes(&block_config);
    let their_pairs: Vec<(String, String)> = router_routes
        .iter()
        .map(|r| (r.path.clone(), r.block.clone()))
        .collect();

    assert_eq!(
        our_pairs, their_pairs,
        "parse_routes_for_validation drift from wafer-block-router::parse_routes:\n  ours: {our_pairs:?}\n  theirs: {their_pairs:?}"
    );
    assert_eq!(our_pairs.len(), 3, "expected 3 routes, got: {our_pairs:?}");
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
                .any(|s| matches!(s, BlockReferenceSource::RouterRoute { .. }));
            assert!(has_flow, "missing Flow source: {:?}", errs[0].sources);
            assert!(
                has_router,
                "missing RouterRoute source: {:?}",
                errs[0].sources
            );
        }
        other => panic!(
            "expected Err(RuntimeError::BlocksNotFound), got {:?}",
            other.as_ref().err()
        ),
    }
}
