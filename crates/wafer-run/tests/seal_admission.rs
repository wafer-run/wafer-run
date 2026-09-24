//! `seal()` admits every block through one gate.
//!
//! - The capabilities an embedder loads a WASM guest with are an upper bound
//!   the guest's own `__wafer_info` declaration cannot widen.
//! - A runtime is sealed once; a second `seal()` is refused rather than
//!   recomputing capabilities without the config narrowing the first consumed.
//! - A block `seal()` downloads from the registry goes through the same
//!   registration checks as a code-registered one (name identity, config-key
//!   prefix, grants), is registered under its unversioned `{org}/{block}`,
//!   and is registered before the grant gate and the capability computation.
//!
//! Guests are WAT modules whose `__wafer_info` returns a `BlockInfo` built
//! here, so each test states exactly what the guest declares. The registry
//! tests serve them from a local wiremock registry, which only the
//! `allow-private-network` build can reach (`scripts/check.sh test` runs this
//! file under that feature too).

#![cfg(feature = "wasm")]

use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use serde_json::json;
use wafer_block::{
    capabilities::{Allowlist, BlockCapabilities},
    core_types::{Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    types::ResourceGrant,
    Block, BlockInfo,
};
use wafer_run::{wasm::WasmiBlock, RuntimeError, Wafer};

const WIDGET: &str = "acme/widget";

fn wafer() -> Wafer {
    Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer")
}

/// A WASM module whose `__wafer_info` reports `info`.
fn guest(info: &BlockInfo) -> Vec<u8> {
    let json = serde_json::to_string(info).expect("BlockInfo serializes");
    let packed = (64u64 << 32) | json.len() as u64;
    let escaped = json.replace('\\', "\\\\").replace('"', "\\\"");
    wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 1)
            (data (i32.const 64) "{escaped}")
            (func (export "__wafer_info") (result i64) (i64.const {packed})))"#
    ))
    .expect("WAT parses")
}

fn widget_info() -> BlockInfo {
    BlockInfo::new(WIDGET, "1.0.0", "handler@v1", "seal admission fixture")
}

fn only(items: &[&str]) -> Allowlist {
    Allowlist::Only(items.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>())
}

/// What the registered block enforces and what the runtime recorded for it.
fn caps_of(w: &Wafer, name: &str) -> (Option<BlockCapabilities>, Option<BlockCapabilities>) {
    let (_, block) = w.lookup_block(name).expect("block is registered");
    (
        block.block_capabilities(),
        w.effective_capabilities(name).cloned(),
    )
}

// ---------------------------------------------------------------------------
// The embedder's capabilities are the bound
// ---------------------------------------------------------------------------

/// A guest loaded with no capabilities declares everything, headers
/// included. After `seal()` it still has none.
#[tokio::test]
async fn a_guest_cannot_declare_past_the_capabilities_it_was_loaded_with() {
    let mut declared = BlockCapabilities::unrestricted();
    declared.headers.readable = vec!["authorization".to_string(), "cookie".to_string()];
    let bytes = guest(&widget_info().capabilities(declared));

    let block =
        WasmiBlock::load_with_capabilities(&bytes, BlockCapabilities::none()).expect("guest loads");
    let mut w = wafer();
    w.register_block(WIDGET, Arc::new(block))
        .expect("registers");
    w.seal().await.expect("seal");

    let none = BlockCapabilities::none();
    assert_eq!(caps_of(&w, WIDGET), (Some(none.clone()), Some(none)));
}

/// Within the bound the declaration still narrows: the guest gets what both
/// the embedder allowed and it asked for.
#[tokio::test]
async fn a_guest_runs_under_the_bound_intersected_with_its_declaration() {
    let mut bound = BlockCapabilities::none();
    bound.collections = only(&["acme__widget__a", "acme__widget__b"]);
    bound.headers.readable = vec!["authorization".to_string()];

    let mut declared = BlockCapabilities::none();
    declared.collections = only(&["acme__widget__a", "acme__widget__c"]);
    declared.crypto = true;
    let bytes = guest(&widget_info().capabilities(declared));

    let block = WasmiBlock::load_with_capabilities(&bytes, bound).expect("guest loads");
    let mut w = wafer();
    w.register_block(WIDGET, Arc::new(block))
        .expect("registers");
    w.seal().await.expect("seal");

    let mut expected = BlockCapabilities::none();
    expected.collections = only(&["acme__widget__a"]);
    assert_eq!(
        caps_of(&w, WIDGET),
        (Some(expected.clone()), Some(expected))
    );
}

/// Guard (passes before the fix by design): a guest loaded without a bound
/// runs under exactly its declaration, header opt-ins included — which
/// `BlockCapabilities::unrestricted()`, what such a guest runs under before
/// `seal()`, does not carry.
#[tokio::test]
async fn an_unbounded_guest_runs_under_its_declaration() {
    let mut declared = BlockCapabilities::none();
    declared.collections = only(&["acme__widget__a"]);
    declared.headers.readable = vec!["authorization".to_string()];
    let bytes = guest(&widget_info().capabilities(declared.clone()));

    let block = WasmiBlock::load_from_bytes(&bytes).expect("guest loads");
    let mut w = wafer();
    w.register_block(WIDGET, Arc::new(block))
        .expect("registers");
    w.seal().await.expect("seal");

    assert_eq!(
        caps_of(&w, WIDGET),
        (Some(declared.clone()), Some(declared))
    );
}

// ---------------------------------------------------------------------------
// Sealed once
// ---------------------------------------------------------------------------

/// The first seal consumes the `capabilities` narrowing from the block's
/// config. A second seal is refused, and the narrowing stays in force.
#[tokio::test]
async fn a_second_seal_is_refused_and_keeps_the_narrowing() {
    let mut declared = BlockCapabilities::none();
    declared.collections = only(&["acme__widget__a", "acme__widget__b"]);
    let bytes = guest(&widget_info().capabilities(declared));

    let block = WasmiBlock::load_from_bytes(&bytes).expect("guest loads");
    let mut w = wafer();
    w.register_block(WIDGET, Arc::new(block))
        .expect("registers");
    w.add_block_config(
        WIDGET,
        json!({ "capabilities": { "collections": { "Only": ["acme__widget__a"] } } }),
    );
    w.seal().await.expect("first seal");
    assert!(w.is_sealed());

    let mut narrowed = BlockCapabilities::none();
    narrowed.collections = only(&["acme__widget__a"]);
    assert_eq!(
        caps_of(&w, WIDGET),
        (Some(narrowed.clone()), Some(narrowed.clone()))
    );

    let second = w.seal().await;
    assert!(
        matches!(second, Err(RuntimeError::AlreadySealed)),
        "a second seal must be refused, got {:?}",
        second.map(|_| "Ok(())")
    );
    assert_eq!(
        caps_of(&w, WIDGET),
        (Some(narrowed.clone()), Some(narrowed))
    );
}

/// A native block declaring a grant over another block's tables.
struct Grants(&'static str, Vec<ResourceGrant>);

#[async_trait]
impl Block for Grants {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.0, "0.1.0", "handler@v1", "grants").grants(self.1.clone())
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::error(WaferError::new(
            wafer_block::ErrorCode::Internal,
            "never dispatched",
        ))
    }
}

/// The grant gate drains the rejections it reports, so a retried seal would
/// find none and boot. A seal that refused boot stays refused.
#[tokio::test]
async fn a_seal_that_refused_boot_cannot_be_retried_into_success() {
    let mut w = wafer();
    w.register_block(
        "x/attacker",
        Arc::new(Grants(
            "x/attacker",
            vec![ResourceGrant::read_write("x/attacker", "a__victim__*")],
        )),
    )
    .expect("registers; the grant is judged at seal");

    assert!(matches!(
        w.seal().await,
        Err(RuntimeError::GrantsRejected(_))
    ));
    assert!(matches!(w.seal().await, Err(RuntimeError::AlreadySealed)));
}

// ---------------------------------------------------------------------------
// Blocks seal() downloads
// ---------------------------------------------------------------------------

#[cfg(feature = "allow-private-network")]
mod downloaded {
    use serial_test::serial;
    use wafer_run::REGISTRY_BASE_URL_KEY;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;

    /// A registry serving `acme/widget` at each `(version, wasm)` pair.
    async fn registry(versions: &[(&str, Vec<u8>)]) -> MockServer {
        let server = MockServer::start().await;
        let mut entries = serde_json::Map::new();
        for (version, wasm) in versions {
            let wasm_path = format!("/acme/widget/{version}/block.wasm");
            entries.insert(
                version.to_string(),
                json!({
                    "abi": wafer_run::ABI_VERSION,
                    "wasm_url": format!("{}{wasm_path}", server.uri()),
                    "flow_url": null,
                }),
            );
            Mock::given(method("GET"))
                .and(path(wasm_path))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(wasm.clone()))
                .mount(&server)
                .await;
        }
        let manifest = json!({
            "name": WIDGET,
            "latest": versions[0].0,
            "versions": entries,
        });
        Mock::given(method("GET"))
            .and(path("/acme/widget/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&manifest))
            .mount(&server)
            .await;
        server
    }

    /// Seal `w` against `server`; the registry URL is process-global, hence
    /// `#[serial]` on every test here.
    async fn seal_against(server: &MockServer, w: &mut Wafer) -> Result<(), RuntimeError> {
        std::env::set_var(REGISTRY_BASE_URL_KEY, server.uri());
        let result = w.seal().await;
        std::env::remove_var(REGISTRY_BASE_URL_KEY);
        result
    }

    fn flow_naming(block: &str) -> wafer_flow::WaferFlow {
        wafer_flow::WaferFlow {
            id: "main".to_string(),
            name: "main".to_string(),
            version: "0.0.1".to_string(),
            description: None,
            input: None,
            output: None,
            steps: vec![wafer_flow::Step {
                id: "s1".to_string(),
                block: block.to_string(),
                input: None,
                next: None,
                each: None,
                parallel: None,
                description: None,
                config: None,
            }],
            config: None,
            blocks: None,
            config_map: None,
            config_defaults: None,
        }
    }

    fn error_of(result: Result<(), RuntimeError>) -> RuntimeError {
        match result {
            Ok(()) => panic!("seal must refuse the downloaded block"),
            Err(e) => e,
        }
    }

    /// A downloaded block declaring another block's secret would be handed
    /// its value in its `Init` payload.
    #[tokio::test]
    #[serial]
    async fn a_downloaded_block_cannot_declare_a_foreign_config_key() {
        let info = widget_info().config_keys(vec![wafer_block::ConfigVar::new(
            "MY_ORG__AUTH__JWT_SECRET",
            "someone else's secret",
            "",
        )]);
        let server = registry(&[("1.0.0", guest(&info))]).await;
        let mut w = wafer();
        w.add_block_config("acme/widget@1.0.0", json!({}));

        let err = error_of(seal_against(&server, &mut w).await);
        match err {
            RuntimeError::InvalidBlockInfo(info_err) => assert_eq!(
                *info_err,
                wafer_block::types::BlockInfoError::ConfigVarPrefix {
                    block: WIDGET.to_string(),
                    key: "MY_ORG__AUTH__JWT_SECRET".to_string(),
                    prefix: "ACME__WIDGET__".to_string(),
                }
            ),
            other => panic!("expected ConfigVarPrefix, got {other}"),
        }
    }

    /// A downloaded block is the name it was fetched as.
    #[tokio::test]
    #[serial]
    async fn a_downloaded_block_reporting_another_name_is_refused() {
        let info = BlockInfo::new("a/victim", "1.0.0", "handler@v1", "spoof");
        let server = registry(&[("1.0.0", guest(&info))]).await;
        let mut w = wafer();
        w.add_block_config("acme/widget@1.0.0", json!({}));

        match error_of(seal_against(&server, &mut w).await) {
            RuntimeError::BlockNameMismatch {
                registered,
                reported,
            } => {
                assert_eq!(registered, WIDGET);
                assert_eq!(reported, "a/victim");
            }
            other => panic!("expected BlockNameMismatch, got {other}"),
        }
    }

    /// A block only a flow step names is downloaded while resolving
    /// references; its grants reach the grant gate like any other block's.
    #[tokio::test]
    #[serial]
    async fn a_step_referenced_download_goes_through_the_grant_gate() {
        let info =
            widget_info().grants(vec![ResourceGrant::read_write("x/reader", "a__victim__*")]);
        let server = registry(&[("1.0.0", guest(&info))]).await;
        let mut w = wafer();
        w.add_flow(flow_naming("acme/widget@1.0.0"));

        match error_of(seal_against(&server, &mut w).await) {
            RuntimeError::GrantsRejected(errors) => assert!(
                errors
                    .iter()
                    .any(|e| e.block == WIDGET && e.grant.resource == "a__victim__*"),
                "the foreign-namespace grant is rejected for {WIDGET}: {errors:?}"
            ),
            other => panic!("expected GrantsRejected, got {other}"),
        }
    }

    /// A step-referenced download gets its effective capabilities computed
    /// like any other block.
    #[tokio::test]
    #[serial]
    async fn a_step_referenced_download_gets_its_declared_capabilities() {
        let mut declared = BlockCapabilities::none();
        declared.collections = only(&["acme__widget__items"]);
        let server = registry(&[(
            "1.0.0",
            guest(&widget_info().capabilities(declared.clone())),
        )])
        .await;
        let mut w = wafer();
        w.add_flow(flow_naming("acme/widget@1.0.0"));

        seal_against(&server, &mut w).await.expect("seal");
        assert_eq!(
            caps_of(&w, WIDGET),
            (Some(declared.clone()), Some(declared))
        );
    }

    /// A versioned reference is registered under the block's unversioned
    /// identity: its own-namespace grants are accepted, the version resolves
    /// as an alias, and the config written under the versioned name —
    /// including its `capabilities` narrowing — is the block's config.
    #[tokio::test]
    #[serial]
    async fn a_versioned_download_is_its_unversioned_name() {
        let mut declared = BlockCapabilities::none();
        declared.collections = only(&["acme__widget__items", "acme__widget__logs"]);
        let grant = ResourceGrant::read("x/reader", "acme__widget__items");
        let info = widget_info()
            .capabilities(declared)
            .grants(vec![grant.clone()]);
        let server = registry(&[("1.0.0", guest(&info))]).await;
        let mut w = wafer();
        w.add_block_config(
            "acme/widget@1.0.0",
            json!({ "capabilities": { "collections": { "Only": ["acme__widget__items"] } } }),
        );

        seal_against(&server, &mut w).await.expect("seal");

        assert_eq!(w.block_names(), vec![WIDGET.to_string()]);
        assert_eq!(w.canonicalize("acme/widget@1.0.0"), WIDGET);
        assert!(
            w.wrap_grants().iter().any(|g| g.resource == grant.resource),
            "the own-namespace grant is collected: {:?}",
            w.wrap_grants()
        );
        let mut narrowed = BlockCapabilities::none();
        narrowed.collections = only(&["acme__widget__items"]);
        assert_eq!(
            caps_of(&w, WIDGET),
            (Some(narrowed.clone()), Some(narrowed))
        );
    }

    /// One runtime holds one version of a block: its tables and config keys
    /// do not depend on the version, so two versions would share them.
    #[tokio::test]
    #[serial]
    async fn two_versions_of_one_block_are_refused() {
        let bytes = guest(&widget_info());
        let server = registry(&[("1.0.0", bytes.clone()), ("2.0.0", bytes)]).await;
        let mut w = wafer();
        w.add_block_config("acme/widget@1.0.0", json!({}));
        w.add_block_config("acme/widget@2.0.0", json!({}));

        match error_of(seal_against(&server, &mut w).await) {
            RuntimeError::DuplicateBlock { name } => assert_eq!(name, WIDGET),
            other => panic!("expected DuplicateBlock, got {other}"),
        }
    }
}
