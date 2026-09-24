//! `seal()` downloads only what `wafer.lock` pins, and only bytes matching
//! the pin.
//!
//! - A reference no lockfile entry pins — a flow step, a block config, with
//!   or without a version — is never fetched, whatever registry is around.
//! - A pinned entry whose cache is missing is fetched from its registry and
//!   refused unless the tarball hashes to the entry's `sha256` and its
//!   `.wasm` to its `wasm_sha256`; an oversized download or a package that
//!   unpacks past the bound is refused before it is buffered or unpacked.
//! - A downloaded block goes through the same registration checks as a
//!   code-registered one (name identity, config-key prefix, grants), is
//!   bounded by the lockfile's `capabilities` or the operator's config, and
//!   is never the admin block.
//!
//! The registry is a local wiremock server, which only the
//! `allow-private-network` build can reach (`scripts/check.sh test` runs
//! this file under that feature).

#![cfg(all(feature = "wasm", feature = "allow-private-network"))]

mod pinned_registry;

use std::{collections::BTreeSet, sync::Arc};

use pinned_registry::{
    build_with_lock, guest, lock_entry, package, requests, seed_cache, serve, tarball,
};
use serde_json::json;
use serial_test::serial;
use wafer_block::{
    capabilities::{Allowlist, BlockCapabilities},
    lockfile::{MAX_PACKAGE_BYTES, MAX_UNPACKED_BYTES},
    types::ResourceGrant,
    BlockInfo,
};
use wafer_run::{wasm::WasmiBlock, RuntimeError, Wafer};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

const WIDGET: &str = "acme/widget";
const VERSION: &str = "1.0.0";

fn widget_info() -> BlockInfo {
    BlockInfo::new(WIDGET, VERSION, "handler@v1", "remote integrity fixture")
}

/// A registry serving `acme/widget@1.0.0` as a package holding `wasm`, and
/// the lockfile entry pinning exactly that package.
async fn widget_registry(wasm: &[u8]) -> (MockServer, String) {
    let server = MockServer::start().await;
    let tarball = package(WIDGET, VERSION, wasm);
    serve(&server, WIDGET, VERSION, tarball.clone()).await;
    let entry = lock_entry(WIDGET, VERSION, &tarball, wasm, &server.uri());
    (server, entry)
}

/// Build from `entries` against an empty cache and seal.
async fn seal_locked(entries: &[String]) -> (Wafer, Result<(), RuntimeError>) {
    let home = tempfile::tempdir().expect("tempdir");
    let mut w = build_with_lock(home.path(), entries).expect("build defers the entries");
    let result = w.seal().await;
    (w, result)
}

fn error_of(result: Result<(), RuntimeError>) -> RuntimeError {
    match result {
        Ok(()) => panic!("seal must refuse"),
        Err(e) => e,
    }
}

fn only(items: &[&str]) -> Allowlist {
    Allowlist::Only(items.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>())
}

fn caps_of(w: &Wafer, name: &str) -> (Option<BlockCapabilities>, Option<BlockCapabilities>) {
    let (_, block) = w.lookup_block(name).expect("block is registered");
    (
        block.block_capabilities(),
        w.effective_capabilities(name).cloned(),
    )
}

// ---------------------------------------------------------------------------
// Nothing unpinned is fetched
// ---------------------------------------------------------------------------

/// Flow steps and block configs naming a registry block — bare, `@latest`,
/// and at a version — with a registry serving it, even through the
/// `WAFER_RUN_REGISTRY_BASE_URL` variable that once selected one: no request
/// is made, and the references are reported as not found.
#[tokio::test]
#[serial]
async fn an_unpinned_reference_is_never_fetched() {
    let server = MockServer::start().await;
    let wasm = guest(&widget_info());
    let manifest = json!({
        "name": WIDGET,
        "latest": VERSION,
        "versions": { VERSION: {
            "abi": 1,
            "wasm_url": format!("{}/acme/widget/{VERSION}/block.wasm", server.uri()),
            "flow_url": null,
        } },
    });
    Mock::given(method("GET"))
        .and(path("/acme/widget/manifest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&manifest))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/acme/widget/{VERSION}/block.wasm")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(wasm.clone()))
        .mount(&server)
        .await;
    serve(&server, WIDGET, VERSION, package(WIDGET, VERSION, &wasm)).await;

    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer");
    for (id, block) in [
        ("bare", "acme/widget"),
        ("latest", "acme/widget@latest"),
        ("versioned", "acme/widget@1.0.0"),
    ] {
        w.add_flow_json(&format!(
            r#"{{"id":"{id}","name":"{id}","version":"0.0.1","steps":[{{"id":"s","block":"{block}"}}]}}"#
        ))
        .expect("flow");
    }
    w.add_block_config("acme/widget@2.0.0", json!({}));
    w.add_block_config("acme/other", json!({}));

    std::env::set_var("WAFER_RUN_REGISTRY_BASE_URL", server.uri());
    let result = w.seal().await;
    std::env::remove_var("WAFER_RUN_REGISTRY_BASE_URL");

    match error_of(result) {
        RuntimeError::BlocksNotFound(missing) => assert_eq!(
            missing.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["acme/widget", "acme/widget@1.0.0", "acme/widget@latest"]
        ),
        other => panic!("expected BlocksNotFound, got {other}"),
    }
    assert_eq!(requests(&server).await, 0, "no registry is contacted");
    assert!(w.block_names().is_empty());
}

// ---------------------------------------------------------------------------
// A pinned download is verified before it is compiled
// ---------------------------------------------------------------------------

/// The pinned package is fetched once, verified and registered under the
/// lockfile name, and flows naming that name resolve.
#[tokio::test]
#[serial]
async fn a_pinned_package_is_fetched_verified_and_registered() {
    let (server, entry) = widget_registry(&guest(&widget_info())).await;
    let home = tempfile::tempdir().expect("tempdir");
    let mut w = build_with_lock(home.path(), &[entry]).expect("build");
    w.add_flow_json(
        r#"{"id":"main","name":"main","version":"0.0.1","steps":[{"id":"s","block":"acme/widget"}]}"#,
    )
    .expect("flow");

    w.seal().await.expect("seal");
    assert_eq!(w.block_names(), vec![WIDGET.to_string()]);
    assert_eq!(requests(&server).await, 1);
}

/// The registry serves a different package for the pinned version than the
/// one `wafer.lock` pins: refused, nothing registered.
#[tokio::test]
#[serial]
async fn a_package_the_lockfile_does_not_pin_is_refused() {
    let pinned = guest(&widget_info());
    let swapped = guest(&BlockInfo::new(WIDGET, VERSION, "handler@v1", "swapped"));
    let server = MockServer::start().await;
    serve(&server, WIDGET, VERSION, package(WIDGET, VERSION, &swapped)).await;
    let entry = lock_entry(
        WIDGET,
        VERSION,
        &package(WIDGET, VERSION, &pinned),
        &pinned,
        &server.uri(),
    );

    let (w, result) = seal_locked(&[entry]).await;
    let msg = error_of(result).to_string();
    assert!(
        msg.contains("integrity check failed") && msg.contains("pins sha256"),
        "{msg}"
    );
    assert!(w.block_names().is_empty(), "nothing is registered");
}

/// The tarball matches its pin but its `.wasm` does not match
/// `wasm_sha256`: refused before the artifact is compiled.
#[tokio::test]
#[serial]
async fn a_wasm_the_lockfile_does_not_pin_is_refused() {
    let wasm = guest(&widget_info());
    let tarball = package(WIDGET, VERSION, &wasm);
    let server = MockServer::start().await;
    serve(&server, WIDGET, VERSION, tarball.clone()).await;
    let entry = lock_entry(
        WIDGET,
        VERSION,
        &tarball,
        b"another artifact",
        &server.uri(),
    );

    let (w, result) = seal_locked(&[entry]).await;
    let msg = error_of(result).to_string();
    assert!(
        msg.contains("integrity check failed") && msg.contains("pins wasm_sha256"),
        "{msg}"
    );
    assert!(w.block_names().is_empty(), "nothing is registered");
}

/// A download past `MAX_PACKAGE_BYTES` is refused on its advertised length.
#[tokio::test]
#[serial]
async fn an_oversized_download_is_refused() {
    let server = MockServer::start().await;
    serve(&server, WIDGET, VERSION, vec![0u8; MAX_PACKAGE_BYTES + 1]).await;
    let entry = lock_entry(WIDGET, VERSION, b"", b"", &server.uri());

    let (_, result) = seal_locked(&[entry]).await;
    let msg = error_of(result).to_string();
    assert!(msg.contains("byte limit"), "{msg}");
}

/// A pinned package that unpacks past `MAX_UNPACKED_BYTES` (a gzip bomb: a
/// small tarball of zeros) is refused while unpacking, not buffered whole.
#[tokio::test]
#[serial]
async fn a_package_unpacking_past_the_bound_is_refused() {
    let zeros = vec![0u8; MAX_UNPACKED_BYTES as usize + 1];
    let bomb = tarball(&[("block.wasm", &zeros)]);
    drop(zeros);
    assert!(bomb.len() < MAX_PACKAGE_BYTES, "the bomb downloads");
    let server = MockServer::start().await;
    serve(&server, WIDGET, VERSION, bomb.clone()).await;
    let entry = lock_entry(WIDGET, VERSION, &bomb, b"", &server.uri());

    let (_, result) = seal_locked(&[entry]).await;
    let msg = error_of(result).to_string();
    assert!(
        msg.contains(&format!("more than {MAX_UNPACKED_BYTES} bytes")),
        "{msg}"
    );
}

// ---------------------------------------------------------------------------
// A downloaded block is admitted like any other
// ---------------------------------------------------------------------------

/// A downloaded block declaring another block's secret would be handed its
/// value in its `Init` payload.
#[tokio::test]
#[serial]
async fn a_downloaded_block_cannot_declare_a_foreign_config_key() {
    let info = widget_info().config_keys(vec![wafer_block::ConfigVar::new(
        "MY_ORG__AUTH__JWT_SECRET",
        "someone else's secret",
        "",
    )]);
    let (_server, entry) = widget_registry(&guest(&info)).await;

    match error_of(seal_locked(&[entry]).await.1) {
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

/// A downloaded block is the name the lockfile pins it under.
#[tokio::test]
#[serial]
async fn a_downloaded_block_reporting_another_name_is_refused() {
    let info = BlockInfo::new("a/victim", VERSION, "handler@v1", "spoof");
    let (_server, entry) = widget_registry(&guest(&info)).await;

    match error_of(seal_locked(&[entry]).await.1) {
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

/// A downloaded block's grants reach the grant gate like any other block's.
#[tokio::test]
#[serial]
async fn a_downloaded_block_goes_through_the_grant_gate() {
    let info = widget_info().grants(vec![ResourceGrant::read_write("x/reader", "a__victim__*")]);
    let (_server, entry) = widget_registry(&guest(&info)).await;

    match error_of(seal_locked(&[entry]).await.1) {
        RuntimeError::GrantsRejected(errors) => assert!(
            errors
                .iter()
                .any(|e| e.block == WIDGET && e.grant.resource == "a__victim__*"),
            "the foreign-namespace grant is rejected for {WIDGET}: {errors:?}"
        ),
        other => panic!("expected GrantsRejected, got {other}"),
    }
}

/// A downloaded block with no lockfile `capabilities` and no operator
/// config runs with none, whatever it declares; with a lockfile bound it
/// runs within the bound.
#[tokio::test]
#[serial]
async fn a_downloaded_block_gets_only_what_the_operator_states() {
    let mut declared = BlockCapabilities::unrestricted();
    declared.headers.readable = vec!["authorization".to_string()];
    let wasm = guest(&widget_info().capabilities(declared));

    let (_server, entry) = widget_registry(&wasm).await;
    let (w, result) = seal_locked(std::slice::from_ref(&entry)).await;
    result.expect("seal");
    let none = BlockCapabilities::none();
    assert_eq!(caps_of(&w, WIDGET), (Some(none.clone()), Some(none)));

    let bounded = format!(
        "{entry}\n[package.capabilities]\ncollections = {{ Only = [\"acme__widget__a\"] }}\n"
    );
    let (w, result) = seal_locked(&[bounded]).await;
    result.expect("seal");
    let mut expected = BlockCapabilities::none();
    expected.collections = only(&["acme__widget__a"]);
    assert_eq!(
        caps_of(&w, WIDGET),
        (Some(expected.clone()), Some(expected))
    );
}

/// The admin block is the one identity WRAP trusts with typed
/// Network/Crypto grants: a lockfile entry naming it is refused, without a
/// download when it is not cached and without loading it when it is.
#[tokio::test]
#[serial]
async fn the_admin_block_is_never_loaded_from_the_lockfile() {
    let wasm = guest(&widget_info());
    let (server, entry) = widget_registry(&wasm).await;
    let home = tempfile::tempdir().expect("tempdir");
    let mut w = build_with_lock(home.path(), std::slice::from_ref(&entry)).expect("build");
    w.set_admin_block(WIDGET);
    let err = error_of(w.seal().await);
    assert!(
        matches!(&err, RuntimeError::Config(m) if m.contains("admin block")),
        "expected the admin-block refusal, got {err}"
    );
    assert_eq!(requests(&server).await, 0, "nothing is fetched");

    let home = tempfile::tempdir().expect("tempdir");
    seed_cache(home.path(), WIDGET, VERSION, &wasm);
    let mut w = build_with_lock(home.path(), &[entry]).expect("build loads the cached entry");
    w.set_admin_block(WIDGET);
    let err = error_of(w.seal().await);
    assert!(
        matches!(&err, RuntimeError::Config(m) if m.contains("admin block")),
        "expected the admin-block refusal, got {err}"
    );
}

/// A pinned entry whose name the embedder registered itself is refused
/// before anything is fetched: one name, one block.
#[tokio::test]
#[serial]
async fn a_pinned_name_the_embedder_registered_is_refused_before_fetching() {
    let wasm = guest(&widget_info());
    let (server, entry) = widget_registry(&wasm).await;
    let home = tempfile::tempdir().expect("tempdir");
    let mut w = build_with_lock(home.path(), &[entry]).expect("build");
    w.register_block(
        WIDGET,
        Arc::new(WasmiBlock::load_from_bytes(&wasm).expect("loads")),
    )
    .expect("registers");

    match error_of(w.seal().await) {
        RuntimeError::DuplicateBlock { name } => assert_eq!(name, WIDGET),
        other => panic!("expected DuplicateBlock, got {other}"),
    }
    assert_eq!(requests(&server).await, 0, "the registry is not contacted");
}

/// A pinned entry whose name is an operator alias is refused, not
/// registered over the alias.
#[tokio::test]
#[serial]
async fn an_operator_alias_is_never_shadowed() {
    let (server, entry) = widget_registry(&guest(&widget_info())).await;
    let home = tempfile::tempdir().expect("tempdir");
    let mut w = build_with_lock(home.path(), &[entry]).expect("build");
    w.add_alias(WIDGET, "x/elsewhere").expect("operator alias");

    let err = error_of(w.seal().await);
    assert!(
        matches!(&err, RuntimeError::Config(m) if m.contains("already an alias")),
        "expected the alias conflict, got {err}"
    );
    assert_eq!(w.canonicalize(WIDGET), "x/elsewhere");
    assert_eq!(requests(&server).await, 0, "the registry is not contacted");
}
