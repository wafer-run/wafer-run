//! Integration tests pinning the security boundary that
//! `examples/with-admin-block` demonstrates. If the example breaks,
//! these tests break. If Wave 13 PR B's validator regresses, these
//! tests break.

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceGrant, ResourceType},
    Block, BlockInfo,
};
use wafer_run::{error::RuntimeError, StaticConfigSource, Wafer};

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// Admin block — its only purpose in this test is to declare the
/// typed Storage grant. Solobase's `suppers-ai/admin` is the production
/// analog (see `solobase-core/src/builder.rs:309`).
struct AdminBlock;

#[async_trait]
impl Block for AdminBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("example/admin", "0.0.1", "admin@v1", "Admin").grants(vec![
            ResourceGrant::read("wafer-run/storage", "*").typed(ResourceType::Storage),
        ])
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
        OutputStream::respond(Vec::new())
    }
}

/// Same typed Storage grant declared from a non-admin block. Wave 13
/// PR B's validator must reject this at `seal()`.
struct NonAdminBlockWithTypedStorageGrant;

#[async_trait]
impl Block for NonAdminBlockWithTypedStorageGrant {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("example/sneaky", "0.0.1", "http-handler@v1", "Sneaky").grants(vec![
            ResourceGrant::read("wafer-run/storage", "*").typed(ResourceType::Storage),
        ])
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
        OutputStream::respond(Vec::new())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_admin_block_admits_typed_storage_grant_via_seal() {
    let mut wafer = Wafer::new(Arc::new(StaticConfigSource::default())).unwrap();
    wafer
        .register_block("example/admin", Arc::new(AdminBlock))
        .expect("register admin");
    wafer.set_admin_block("example/admin");
    wafer
        .seal()
        .await
        .expect("seal admits admin-declared typed Storage grant");
}

#[tokio::test]
async fn typed_storage_grant_from_non_admin_block_rejected_via_seal() {
    let mut wafer = Wafer::new(Arc::new(StaticConfigSource::default())).unwrap();
    // Admin block is set, but the typed grant lives on a DIFFERENT
    // block. Wave 13 PR B's validator must reject this at seal().
    wafer
        .register_block("example/admin", Arc::new(AdminBlock))
        .expect("register admin");
    wafer
        .register_block(
            "example/sneaky",
            Arc::new(NonAdminBlockWithTypedStorageGrant),
        )
        .expect("register sneaky");
    wafer.set_admin_block("example/admin");

    let err = wafer
        .seal()
        .await
        .expect_err("seal must reject typed grant from non-admin block");
    match err {
        RuntimeError::GrantsRejected(errs) => {
            assert!(
                errs.iter().any(|e| e.block == "example/sneaky"),
                "expected rejection list to include `example/sneaky`, got {errs:?}",
            );
        }
        other => panic!("expected RuntimeError::GrantsRejected, got {other:?}"),
    }
}
