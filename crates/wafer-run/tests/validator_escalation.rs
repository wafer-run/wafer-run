//! Integration tests for Wave 13 PR B: Wafer::start() must refuse boot
//! when validate_and_collect_grants_for_block has rejected one or more
//! typed grants. Replaces the prior silent tracing::error! behavior.
//!
//! Wave 26 (c18) changed the rejection reason for typed Storage grants
//! from "admin-only" to "namespace ownership violation". These tests
//! still assert the escalation path, but the fixtures now use the
//! c18-shaped failure: a block declaring a Storage grant for a resource
//! it doesn't own (foreign-namespace) gets rejected.

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceGrant, ResourceType},
    Block, BlockInfo,
};
use wafer_run::{RuntimeError, StaticConfigSource, Wafer};

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// Block that declares a typed Storage grant for a resource it doesn't
/// own (`other/block/*`). After Wave 26 the validator rejects via the
/// ownership check.
struct OffendingBlock;

#[async_trait]
impl Block for OffendingBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/offender",
            "0.0.1",
            "test/iface@v1",
            "block declaring Storage grant for foreign namespace",
        )
        .grants(vec![ResourceGrant::read_write(
            "test/offender",
            "other/block/*",
        )
        .typed(ResourceType::Storage)])
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

/// Admin block declaring a typed Storage grant within its own namespace
/// (`test/admin/...`). The validator accepts via the ownership check.
struct AdminBlock;

#[async_trait]
impl Block for AdminBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/admin",
            "0.0.1",
            "test/iface@v1",
            "admin block with typed Storage grant in own namespace",
        )
        .grants(vec![ResourceGrant::read_write(
            "test/admin",
            "test/admin/sub/*",
        )
        .typed(ResourceType::Storage)])
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
async fn validator_rejects_typed_storage_from_non_admin_block_fails_start() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    // Set admin to something else, so the offending block is definitely non-admin.
    wafer.set_admin_block("test/admin");
    wafer
        .register_block("test/offender", Arc::new(OffendingBlock))
        .expect("register_block must succeed even for rejected grants");

    let result = wafer.start().await;
    match result {
        Err(RuntimeError::GrantsRejected(errors)) => {
            assert_eq!(errors.len(), 1, "expected one rejection, got: {errors:?}");
            assert_eq!(errors[0].block, "test/offender");
            // After Wave 26 the rejection reason mentions ownership;
            // the resource is `other/block/*`, owned by `other/block`.
            assert!(
                errors[0].reason.contains("other/block"),
                "reason should mention the owner (`other/block`): got `{}`",
                errors[0].reason
            );
        }
        other => panic!(
            "expected Err(RuntimeError::GrantsRejected), got {:?}",
            other.map(|_| "Ok(_)")
        ),
    }
}

#[tokio::test]
async fn validator_accepts_typed_grant_from_admin_block_passes_start() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.set_admin_block("test/admin");
    wafer
        .register_block("test/admin", Arc::new(AdminBlock))
        .expect("register_block");

    let result = wafer.start().await;
    assert!(
        result.is_ok(),
        "start() should succeed when typed grants are declared by the admin block, got Err: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn validator_rejects_typed_grant_via_direct_seal_call() {
    // Cloudflare and browser consumers call seal() directly without start().
    // This test locks in that the drain fires on the seal() path, not only
    // on start_with_priority() — fixing the C1 code-review finding on Wave 13 PR B.
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.set_admin_block("test/admin");
    wafer
        .register_block("test/offender", Arc::new(OffendingBlock))
        .expect("register_block must succeed even for rejected grants");

    let result = wafer.seal().await;
    match result {
        Err(RuntimeError::GrantsRejected(errors)) => {
            assert_eq!(errors.len(), 1, "expected one rejection, got: {errors:?}");
            assert_eq!(errors[0].block, "test/offender");
        }
        other => panic!(
            "expected Err(RuntimeError::GrantsRejected), got {:?}",
            other.map(|_| "Ok(_)")
        ),
    }
}
