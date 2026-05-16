//! WRAP grants are collected at register_block time, not at start/init.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §1

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceGrant, ResourceType},
    Block, BlockInfo,
};
use wafer_run::{Context, StaticConfigSource, Wafer};

struct GrantingBlock {
    name: &'static str,
    grants: Vec<ResourceGrant>,
}

#[async_trait]
impl Block for GrantingBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "test").grants(self.grants.clone())
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        panic!("registration must not invoke lifecycle");
    }
    async fn handle(&self, _ctx: &dyn Context, _m: Message, _input: InputStream) -> OutputStream {
        panic!("registration must not invoke handle");
    }
}

#[tokio::test]
async fn grants_visible_immediately_after_register() {
    // For block "test/granter", the namespace prefix is "test__granter__"
    // so resource "test__granter__foo" is owned by it and validates ok.
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block(
            "test/granter",
            Arc::new(GrantingBlock {
                name: "test/granter",
                grants: vec![ResourceGrant::read("*", "test__granter__foo")],
            }),
        )
        .expect("register");

    // No init, no resolve, no dispatch — grants must already be visible.
    let grants = wafer.wrap_grants();
    assert_eq!(grants.len(), 1, "expected 1 grant, got {grants:?}");
    assert_eq!(grants[0].resource, "test__granter__foo");
}

#[tokio::test]
async fn unowned_namespace_grant_is_dropped() {
    // Block "test/granter" tries to grant access to "test__other__foo" —
    // not owned by it. Per existing security validation, the grant is
    // logged and dropped. register_block must still succeed.
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block(
            "test/granter",
            Arc::new(GrantingBlock {
                name: "test/granter",
                grants: vec![ResourceGrant::read("*", "test__other__foo")],
            }),
        )
        .expect("register");

    let grants = wafer.wrap_grants();
    assert!(
        grants.is_empty(),
        "non-owned grant must be dropped, got {grants:?}"
    );
}

#[tokio::test]
async fn typed_grant_from_admin_block_is_kept() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.set_admin_block("suppers-ai/admin");
    wafer
        .register_block(
            "suppers-ai/admin",
            Arc::new(GrantingBlock {
                name: "suppers-ai/admin",
                grants: vec![
                    ResourceGrant::read("*", "https://example.com").typed(ResourceType::Network)
                ],
            }),
        )
        .expect("register");

    let grants = wafer.wrap_grants();
    assert_eq!(grants.len(), 1, "admin's typed grant must be kept");
    assert_eq!(grants[0].resource, "https://example.com");
}

#[tokio::test]
async fn typed_grant_from_non_admin_is_dropped() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.set_admin_block("suppers-ai/admin");
    // Non-admin block declares a typed Network grant — must be dropped.
    wafer
        .register_block(
            "test/granter",
            Arc::new(GrantingBlock {
                name: "test/granter",
                grants: vec![
                    ResourceGrant::read("*", "https://example.com").typed(ResourceType::Network)
                ],
            }),
        )
        .expect("register");

    let grants = wafer.wrap_grants();
    assert!(
        grants.is_empty(),
        "non-admin typed grant must be dropped, got {grants:?}"
    );
}

#[tokio::test]
async fn typed_grant_without_admin_set_returns_error() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    // wrap_admin_block is unset (empty). A typed grant must fail loud.
    let result = wafer.register_block(
        "test/granter",
        Arc::new(GrantingBlock {
            name: "test/granter",
            grants: vec![
                ResourceGrant::read("*", "https://example.com").typed(ResourceType::Network)
            ],
        }),
    );

    let err = result.expect_err("expected WrapGrantAdminUnset");
    let msg = err.to_string();
    assert!(
        msg.contains("test/granter") && msg.contains("admin"),
        "error must mention block and admin: {msg}"
    );
}

#[tokio::test]
async fn add_wrap_grants_appends_after_register() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block(
            "test/granter",
            Arc::new(GrantingBlock {
                name: "test/granter",
                grants: vec![ResourceGrant::read("*", "test__granter__foo")],
            }),
        )
        .expect("register");

    // External grants (e.g., from DB) still append on top.
    wafer.add_wrap_grants(vec![ResourceGrant::read("test/other", "external/thing")]);
    let grants = wafer.wrap_grants();
    assert_eq!(grants.len(), 2, "got {grants:?}");
    assert_eq!(grants[0].resource, "test__granter__foo");
    assert_eq!(grants[1].resource, "external/thing");
}
