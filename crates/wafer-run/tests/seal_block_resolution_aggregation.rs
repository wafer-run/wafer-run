//! Integration tests for Wave 16 PR A: Wafer::seal() must aggregate every
//! missing block reference into one RuntimeError::BlocksNotFound instead
//! of failing fast on the first one.

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    error::{BlockReferenceError, BlockReferenceSource},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo,
};
use wafer_run::{error::RuntimeError, StaticConfigSource, Wafer};

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// Minimal block that satisfies the registration contract; never invoked
/// because seal() should fail before any dispatch happens.
struct NoopBlock(&'static str);

#[async_trait]
impl Block for NoopBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            self.0,
            "0.0.1",
            "test/iface@v1",
            "noop block for seal() tests",
        )
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

#[tokio::test]
async fn seal_aggregates_multiple_missing_block_refs() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.add_flow(flow_with_steps(
        "my-flow",
        vec![
            step("call-a", "example/missing-a"),
            step("call-b", "example/missing-b"),
        ],
    ));

    let result = wafer.seal().await;
    match result {
        Err(RuntimeError::BlocksNotFound(errs)) => {
            assert_eq!(
                errs.len(),
                2,
                "expected two missing-block entries, got: {errs:?}"
            );
            assert!(
                errs.iter().any(|e| e.name == "example/missing-a"),
                "missing example/missing-a entry: {errs:?}"
            );
            assert!(
                errs.iter().any(|e| e.name == "example/missing-b"),
                "missing example/missing-b entry: {errs:?}"
            );
        }
        other => panic!(
            "expected Err(RuntimeError::BlocksNotFound), got {:?}",
            other.as_ref().err()
        ),
    }
}

#[tokio::test]
async fn seal_aggregates_multiple_refs_to_same_missing_block() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.add_flow(flow_with_steps(
        "my-flow",
        vec![
            step("first", "example/missing"),
            step("second", "example/missing"),
        ],
    ));

    let result = wafer.seal().await;
    match result {
        Err(RuntimeError::BlocksNotFound(errs)) => {
            assert_eq!(
                errs.len(),
                1,
                "expected one missing-block entry, got: {errs:?}"
            );
            let entry: &BlockReferenceError = &errs[0];
            assert_eq!(entry.name, "example/missing");
            assert_eq!(
                entry.sources.len(),
                2,
                "expected two sources for the same missing block: {entry:?}"
            );
            let step_indices: Vec<usize> = entry
                .sources
                .iter()
                .map(|s| match s {
                    BlockReferenceSource::Flow { step_index, .. } => *step_index,
                    other => panic!("expected Flow source, got {other:?}"),
                })
                .collect();
            assert!(
                step_indices.contains(&0),
                "missing step 0: {step_indices:?}"
            );
            assert!(
                step_indices.contains(&1),
                "missing step 1: {step_indices:?}"
            );
        }
        other => panic!(
            "expected Err(RuntimeError::BlocksNotFound), got {:?}",
            other.as_ref().err()
        ),
    }
}

#[tokio::test]
async fn seal_succeeds_when_all_flow_block_refs_resolve() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block("example/present", Arc::new(NoopBlock("example/present")))
        .expect("register_block");
    wafer.add_flow(flow_with_steps(
        "my-flow",
        vec![step("only", "example/present")],
    ));

    let result = wafer.seal().await;
    assert!(
        result.is_ok(),
        "seal() should succeed when all flow steps reference registered blocks, got Err: {:?}",
        result.err()
    );
}
