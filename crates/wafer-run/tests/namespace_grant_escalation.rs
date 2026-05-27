//! Integration tests for Wave 15 PR A: Wafer::seal() must refuse boot
//! when validate_and_collect_grants_for_block has rejected one or more
//! namespace-based grants (resource owned by another block, or
//! unnamespaced). Symmetric extension of Wave 13 PR B
//! (validator_escalation.rs), which covered typed grants only.

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    types::ResourceGrant,
    Block, BlockInfo,
};
use wafer_run::{error::RuntimeError, StaticConfigSource, Wafer};

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// Block that declares a namespace grant for a resource owned by another
/// block. Will be rejected by validate_and_collect_grants_for_block with
/// `Some(owner)` where owner != block_info.name.
struct OtherOwnerOffender;

#[async_trait]
impl Block for OtherOwnerOffender {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "example/foo",
            "0.0.1",
            "test/iface@v1",
            "block declaring grant for a resource owned by example/bar",
        )
        .grants(vec![ResourceGrant::read_write(
            "example/foo",
            "example__bar__*",
        )])
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

/// Block that declares a namespace grant for a bare, unnamespaced resource.
/// Will be rejected by validate_and_collect_grants_for_block with
/// `None` for grant_owner.
struct UnnamespacedOffender;

#[async_trait]
impl Block for UnnamespacedOffender {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "example/foo",
            "0.0.1",
            "test/iface@v1",
            "block declaring grant for an unnamespaced resource",
        )
        .grants(vec![ResourceGrant::read_write(
            "example/foo",
            "legacy_table_no_prefix",
        )])
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

/// Block that declares a correctly-namespaced grant for a resource it owns.
/// Validation should accept it; seal() should return Ok(()).
struct CorrectlyNamespacedBlock;

#[async_trait]
impl Block for CorrectlyNamespacedBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "example/foo",
            "0.0.1",
            "test/iface@v1",
            "block declaring a grant for a resource it owns",
        )
        .grants(vec![ResourceGrant::read_write(
            "example/foo",
            "example__foo__widgets",
        )])
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
        OutputStream::respond(b"ok".to_vec())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn namespace_violation_grant_for_other_owner_rejected_via_seal() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block("example/foo", Arc::new(OtherOwnerOffender))
        .expect("register_block must succeed even for rejected grants");

    let result = wafer.seal().await;
    match result {
        Err(RuntimeError::GrantsRejected(errors)) => {
            assert!(
                errors.iter().any(|e| e.block == "example/foo"),
                "expected rejection from example/foo, got: {errors:?}",
            );
            assert!(
                errors.iter().any(|e| e.reason.contains("example__bar__")
                    && e.reason.contains("not by declaring block")),
                "reason should name owning block and explain mismatch: {errors:?}",
            );
        }
        other => panic!(
            "expected Err(RuntimeError::GrantsRejected), got {:?}",
            other.map(|_| "Ok(_)"),
        ),
    }
}

#[tokio::test]
async fn unnamespaced_grant_rejected_via_seal() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block("example/foo", Arc::new(UnnamespacedOffender))
        .expect("register_block must succeed even for rejected grants");

    let result = wafer.seal().await;
    match result {
        Err(RuntimeError::GrantsRejected(errors)) => {
            assert!(
                errors.iter().any(|e| e.block == "example/foo"),
                "expected rejection from example/foo, got: {errors:?}",
            );
            assert!(
                errors.iter().any(|e| e.reason.contains("unnamespaced")
                    && e.reason.contains("legacy_table_no_prefix")),
                "reason should flag the unnamespaced resource by name: {errors:?}",
            );
        }
        other => panic!(
            "expected Err(RuntimeError::GrantsRejected), got {:?}",
            other.map(|_| "Ok(_)"),
        ),
    }
}

#[tokio::test]
async fn correctly_namespaced_grant_admitted_via_seal() {
    // Positive control: properly-owned namespace grant should NOT trigger
    // RuntimeError::GrantsRejected. Locks in that the new rejection paths
    // didn't accidentally reject the happy case.
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block("example/foo", Arc::new(CorrectlyNamespacedBlock))
        .expect("register_block");

    let result = wafer.seal().await;
    assert!(
        result.is_ok(),
        "seal() should succeed when the namespace grant is correctly owned by the declaring block, got Err: {:?}",
        result.err(),
    );
}
