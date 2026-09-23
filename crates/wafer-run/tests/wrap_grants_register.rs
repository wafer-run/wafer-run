//! WRAP grants are collected at register_block time, not at start/init.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §1

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    types::{GrantWrite, ResourceGrant, ResourceType},
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
    wafer.set_admin_block("my-org/admin");
    wafer
        .register_block(
            "my-org/admin",
            Arc::new(GrantingBlock {
                name: "my-org/admin",
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
    wafer.set_admin_block("my-org/admin");
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
async fn typed_grant_without_admin_set_is_deferred_until_set_admin_block() {
    // This is the linkme-registration scenario used by the consuming application: blocks
    // with typed WRAP grants are registered before the embedder calls
    // `set_admin_block`. Registration must NOT fail; instead, the grant
    // is deferred and then re-collected when `set_admin_block` runs.
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    // wrap_admin_block is unset (empty) at registration time.
    wafer
        .register_block(
            "my-org/admin",
            Arc::new(GrantingBlock {
                name: "my-org/admin",
                grants: vec![
                    ResourceGrant::read("*", "https://example.com").typed(ResourceType::Network)
                ],
            }),
        )
        .expect("registration must succeed even before set_admin_block");

    // Immediately after registration, the typed grant is deferred.
    assert!(
        wafer.wrap_grants().is_empty(),
        "typed grant must be deferred while admin block is unset: {:?}",
        wafer.wrap_grants()
    );

    // Setting the admin block re-collects deferred typed grants.
    wafer.set_admin_block("my-org/admin");
    let grants = wafer.wrap_grants();
    assert_eq!(
        grants.len(),
        1,
        "set_admin_block must re-collect the deferred typed grant, got {grants:?}"
    );
    assert_eq!(grants[0].resource, "https://example.com");
}

#[tokio::test]
async fn set_admin_block_preserves_external_grants() {
    // External grants (added via `add_wrap_grants`) must survive a
    // `set_admin_block` rescan — they are not block-declared and have
    // no way to be re-derived.
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block(
            "my-org/admin",
            Arc::new(GrantingBlock {
                name: "my-org/admin",
                grants: vec![
                    ResourceGrant::read("*", "https://example.com").typed(ResourceType::Network)
                ],
            }),
        )
        .expect("register");
    wafer
        .add_wrap_grants(vec![ResourceGrant::read("test/other", "external/thing")])
        .expect("a well-formed external grant is added");

    // Setting admin block triggers a rescan that must keep both grants.
    wafer.set_admin_block("my-org/admin");
    let grants = wafer.wrap_grants();
    let resources: Vec<&str> = grants.iter().map(|g| g.resource.as_str()).collect();
    assert!(
        resources.contains(&"https://example.com"),
        "admin's typed grant should be present after rescan: {resources:?}"
    );
    assert!(
        resources.contains(&"external/thing"),
        "external grant must be preserved across rescan: {resources:?}"
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
    wafer
        .add_wrap_grants(vec![ResourceGrant::read("test/other", "external/thing")])
        .expect("a well-formed external grant is added");
    let grants = wafer.wrap_grants();
    assert_eq!(grants.len(), 2, "got {grants:?}");
    assert_eq!(grants[0].resource, "test__granter__foo");
    assert_eq!(grants[1].resource, "external/thing");
}

#[tokio::test]
async fn append_grant_on_own_collection_is_kept() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block(
            "test/granter",
            Arc::new(GrantingBlock {
                name: "test/granter",
                grants: vec![ResourceGrant::append("test/writer", "test__granter__audit")],
            }),
        )
        .expect("register");

    let grants = wafer.wrap_grants();
    assert_eq!(grants.len(), 1, "append grant must be kept, got {grants:?}");
    assert_eq!(grants[0].write, GrantWrite::Append);
    wafer
        .seal()
        .await
        .expect("a well-formed append grant seals");
}

/// An append-only grant not typed `Db` is rejected at registration and
/// fails `seal()` — it never reaches the WRAP check.
#[tokio::test]
async fn append_grants_not_typed_db_are_rejected_via_seal() {
    let untyped = ResourceGrant {
        resource_type: None,
        ..ResourceGrant::append("test/writer", "test__granter__audit")
    };
    let storage =
        ResourceGrant::append("test/writer", "test/granter/logs").typed(ResourceType::Storage);
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block(
            "test/granter",
            Arc::new(GrantingBlock {
                name: "test/granter",
                grants: vec![untyped, storage],
            }),
        )
        .expect("register_block must succeed even for rejected grants");

    assert!(
        wafer.wrap_grants().is_empty(),
        "no malformed grant may be installed, got {:?}",
        wafer.wrap_grants()
    );
    match wafer.seal().await {
        Err(wafer_run::RuntimeError::GrantsRejected(errors)) => {
            let reasons: Vec<&str> = errors.iter().map(|e| e.reason.as_str()).collect();
            assert_eq!(errors.len(), 2, "{reasons:?}");
            assert!(
                reasons
                    .iter()
                    .all(|r| r.contains("database collections only")),
                "{reasons:?}"
            );
        }
        other => panic!(
            "expected Err(RuntimeError::GrantsRejected), got {:?}",
            other.map(|_| "Ok(_)")
        ),
    }
}

/// `add_wrap_grants` skips the per-block ownership rules, but not the shape
/// check: a malformed grant fails the call and nothing from it is installed.
#[tokio::test]
async fn add_wrap_grants_rejects_malformed_grants_whole() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    let untyped = ResourceGrant {
        resource_type: None,
        ..ResourceGrant::append("test/writer", "test__granter__audit")
    };
    match wafer.add_wrap_grants(vec![
        ResourceGrant::read("test/other", "external/thing"),
        untyped,
    ]) {
        Err(wafer_run::RuntimeError::GrantsRejected(errors)) => {
            assert_eq!(errors.len(), 1, "{errors:?}");
            assert!(errors[0].reason.contains("database collections only"));
        }
        other => panic!("expected GrantsRejected, got {other:?}"),
    }
    assert!(
        wafer.wrap_grants().is_empty(),
        "a rejected call installs nothing, got {:?}",
        wafer.wrap_grants()
    );
    wafer.seal().await.expect("nothing malformed was installed");
}
