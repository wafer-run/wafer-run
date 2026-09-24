//! `seal()` admits every block through one gate.
//!
//! - The capabilities an embedder loads a WASM guest with are an upper bound
//!   the guest's own `__wafer_info` declaration cannot widen.
//! - A runtime is sealed once; a second `seal()` is refused rather than
//!   recomputing capabilities without the config narrowing the first consumed.
//!
//! A block `seal()` downloads is admitted through the same gate; its tests
//! live in `remote_integrity.rs`.
//!
//! Guests are WAT modules whose `__wafer_info` returns a `BlockInfo` built
//! here, so each test states exactly what the guest declares.

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

/// A guest no embedder bounded gets what the operator states in its
/// `capabilities` config, ∩ its declaration — and with no statement,
/// nothing: its declaration is a request, not a grant.
#[tokio::test]
async fn an_unbounded_guest_gets_only_what_the_operator_states() {
    let mut declared = BlockCapabilities::unrestricted();
    declared.headers.readable = vec!["authorization".to_string(), "cookie".to_string()];
    let bytes = guest(&widget_info().capabilities(declared));

    let mut w = wafer();
    w.register_block(
        WIDGET,
        Arc::new(WasmiBlock::load_from_bytes(&bytes).expect("loads")),
    )
    .expect("registers");
    w.seal().await.expect("seal");
    let none = BlockCapabilities::none();
    assert_eq!(caps_of(&w, WIDGET), (Some(none.clone()), Some(none)));

    let mut w = wafer();
    w.register_block(
        WIDGET,
        Arc::new(WasmiBlock::load_from_bytes(&bytes).expect("loads")),
    )
    .expect("registers");
    w.add_block_config(
        WIDGET,
        json!({ "capabilities": {
            "collections": { "Only": ["acme__widget__a"] },
            "headers": { "readable": ["authorization"] },
        } }),
    );
    w.seal().await.expect("seal");
    let mut stated = BlockCapabilities::none();
    stated.collections = only(&["acme__widget__a"]);
    stated.headers.readable = vec!["authorization".to_string()];
    assert_eq!(caps_of(&w, WIDGET), (Some(stated.clone()), Some(stated)));
}

/// An embedder that vetted a guest approves exactly its declaration, header
/// opt-ins included.
#[tokio::test]
async fn an_embedder_can_approve_the_declaration() {
    let mut declared = BlockCapabilities::none();
    declared.collections = only(&["acme__widget__a"]);
    declared.headers.readable = vec!["authorization".to_string()];
    let bytes = guest(&widget_info().capabilities(declared.clone()));

    let block =
        WasmiBlock::load_approving_declaration(&bytes, wafer_run::ResourceLimits::default())
            .expect("guest loads");
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
    assert_eq!(w.seal_state(), &wafer_run::SealState::Sealed);

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

    let first = w.seal().await;
    assert!(matches!(first, Err(RuntimeError::GrantsRejected(_))));
    assert!(matches!(w.seal().await, Err(RuntimeError::AlreadySealed)));
    assert_eq!(
        w.seal_state(),
        &wafer_run::SealState::Failed(first.unwrap_err().to_string()),
        "the failure is kept for an embedder's start() to re-report"
    );
}

/// The terminal error of `out`, which must be one.
async fn error_terminal(out: OutputStream) -> WaferError {
    match out.collect_buffered().await {
        Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => e,
        _ => panic!("dispatch must be refused with an error"),
    }
}

/// Dispatch runs only on a runtime `seal()` sealed: an unsealed one would
/// run blocks under their load-time capabilities and no grant gate, and a
/// failed seal left the runtime half-built.
#[tokio::test]
async fn dispatch_is_refused_unless_the_seal_succeeded() {
    let bytes = guest(&widget_info());
    let mut w = wafer();
    w.register_block(
        WIDGET,
        Arc::new(WasmiBlock::load_from_bytes(&bytes).expect("loads")),
    )
    .expect("registers");

    let err = error_terminal(
        w.run_block(WIDGET, Message::new("x"), InputStream::empty())
            .await,
    )
    .await;
    assert_eq!(err.code, wafer_block::ErrorCode::FailedPrecondition);
    assert!(err.message.contains("not sealed"), "{}", err.message);
    let err = error_terminal(w.run("main", Message::new("x"), InputStream::empty()).await).await;
    assert_eq!(err.code, wafer_block::ErrorCode::FailedPrecondition);

    w.register_block(
        "x/attacker",
        Arc::new(Grants(
            "x/attacker",
            vec![ResourceGrant::read_write("x/attacker", "a__victim__*")],
        )),
    )
    .expect("registers");
    assert!(w.seal().await.is_err());
    let err = error_terminal(
        w.run_block(WIDGET, Message::new("x"), InputStream::empty())
            .await,
    )
    .await;
    assert_eq!(err.code, wafer_block::ErrorCode::FailedPrecondition);
    assert!(err.message.contains("failed to seal"), "{}", err.message);
}
