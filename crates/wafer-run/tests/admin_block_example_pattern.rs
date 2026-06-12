//! Integration tests pinning the WRAP-grant ownership boundary for
//! Storage grants after Wave 26 (c18).
//!
//! Before Wave 26, typed Storage grants were admin-only: only the
//! configured admin block could declare them, and the resource pattern
//! was unrestricted. Wave 13 PR B added the admin-only validator;
//! Wave 26 (c18) replaced it with namespace ownership — Storage paths
//! follow `{org}/{block}/...`, blocks self-admit their own namespace
//! via WRAP Rule 3, and any block can declare cross-block Storage
//! grants for resources in its OWN namespace (just like Db grants).
//!
//! These tests pin three guarantees:
//! 1. A block declaring a Storage grant for its own namespace is accepted.
//! 2. A block declaring a Storage grant for a foreign namespace is rejected.
//! 3. A block declaring an unnamespaced Storage grant (e.g. resource `*`)
//!    is rejected as un-attributable.
//!
//! If Wave 26's validator regresses these tests break.

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

/// Block `example/owner` that declares a Storage grant for a resource
/// within its OWN namespace (`example/owner/...`). After Wave 26 this
/// is the valid form for typed Storage grants — owners attribute access
/// to grantees for resources they own.
struct OwnerBlockWithOwnNamespaceGrant;

#[async_trait]
impl Block for OwnerBlockWithOwnNamespaceGrant {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("example/owner", "0.0.1", "http-handler@v1", "Owner").grants(vec![
            ResourceGrant::read("example/grantee", "example/owner/sub/*")
                .typed(ResourceType::Storage),
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

/// Block `example/foo` declaring a Storage grant for a resource that
/// belongs to a DIFFERENT block (`example/bar/*`). After Wave 26's
/// ownership check this must be rejected — a block can only attribute
/// access to resources it owns.
struct ForeignNamespaceGrantBlock;

#[async_trait]
impl Block for ForeignNamespaceGrantBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("example/foo", "0.0.1", "http-handler@v1", "Foo").grants(vec![
            ResourceGrant::read("example/foo", "example/bar/*").typed(ResourceType::Storage),
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

/// Block declaring a wildcard Storage grant (resource `*`). With Wave 26
/// the resource has no namespace owner; the validator rejects as
/// un-attributable. Pre-Wave-26 this was admin-only (the admin block was
/// permitted to declare unscoped grants); post-c18 there is no escape
/// hatch — Storage grants must name a `{org}/{block}/...`-shaped resource.
struct UnnamespacedStorageGrantBlock;

#[async_trait]
impl Block for UnnamespacedStorageGrantBlock {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn typed_storage_grant_within_own_namespace_admitted() {
    let mut wafer = Wafer::new(Arc::new(StaticConfigSource::default())).unwrap();
    wafer
        .register_block("example/owner", Arc::new(OwnerBlockWithOwnNamespaceGrant))
        .expect("register owner");
    wafer.set_admin_block("example/owner");
    wafer
        .seal()
        .await
        .expect("seal admits Storage grant for resource within declaring block's namespace");
}

#[tokio::test]
async fn typed_storage_grant_for_foreign_namespace_rejected() {
    let mut wafer = Wafer::new(Arc::new(StaticConfigSource::default())).unwrap();
    wafer
        .register_block("example/foo", Arc::new(ForeignNamespaceGrantBlock))
        .expect("register foo");
    wafer.set_admin_block("example/foo");

    let err = wafer
        .seal()
        .await
        .expect_err("seal must reject Storage grant for resource not owned by declaring block");
    match err {
        RuntimeError::GrantsRejected(errs) => {
            assert!(
                errs.iter().any(|e| e.block == "example/foo"),
                "expected rejection list to include `example/foo`, got {errs:?}",
            );
        }
        other => panic!("expected RuntimeError::GrantsRejected, got {other:?}"),
    }
}

#[tokio::test]
async fn typed_storage_grant_with_unnamespaced_resource_rejected() {
    let mut wafer = Wafer::new(Arc::new(StaticConfigSource::default())).unwrap();
    wafer
        .register_block("example/admin", Arc::new(UnnamespacedStorageGrantBlock))
        .expect("register admin");
    wafer.set_admin_block("example/admin");

    let err = wafer
        .seal()
        .await
        .expect_err("seal must reject Storage grant with unnamespaced `*` resource");
    match err {
        RuntimeError::GrantsRejected(errs) => {
            assert!(
                errs.iter().any(|e| e.block == "example/admin"),
                "expected rejection list to include `example/admin`, got {errs:?}",
            );
        }
        other => panic!("expected RuntimeError::GrantsRejected, got {other:?}"),
    }
}
