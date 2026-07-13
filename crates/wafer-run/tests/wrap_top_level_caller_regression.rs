//! Task 8 regression: the *real* top-level HTTP → service call chain must
//! not be denied by SP-A Stage 1's host-side `check_resource_access`.
//!
//! ## The T2 review finding this closes
//!
//! `Wafer::make_context` (`runtime.rs`) always sets `caller_id: None` for
//! every top-level frame — the HTTP listener's own dispatch, each flow
//! step, and `lifecycle(Init/Start/Stop)`. `wafer_block::wrap::check_access`
//! denies every rule that requires `Some(caller)` (own-resource, admin,
//! grant-match) when `caller_id` is `None`. If a *legitimate* top-level path
//! ever reached `RuntimeContext::check_resource_access` with `caller_id:
//! None`, Stage 1 would newly deny it — a regression.
//!
//! ## Why it doesn't happen (verified against the real topology)
//!
//! Resource-access handlers (database/storage/config/network/crypto) are
//! never the *direct* top-level dispatch target in production. The consuming
//! application's real chain (`wafer-run/http-listener` → flow `site-main` →
//! `wafer-run/router` → domain block → `wafer-run/{sqlite,s3,...}`) always
//! puts at least one attributable hop between the top-level frame and any
//! resource-owning block, because:
//!
//! - `RuntimeContext::dispatch_call` (the `ctx.call_block()` implementation)
//!   stamps the CALLEE's `caller_id` from the CALLER's `node_id` — i.e.
//!   `caller_id: Some(self.node_id.clone())` — not from the caller's own
//!   `caller_id`. So a `None`-caller_id top-level frame (`wafer-run/router`,
//!   a flow-step middleware block, or a block's own `Init`) that itself
//!   calls `ctx.call_block(next)` still hands `next` an attributable
//!   `Some("wafer-run/router")` / `Some(migrating_block_name)`.
//! - `wafer-run/router` (`wafer-block-router`) only ever forwards via
//!   `ctx.call_block()` (`lib.rs:192`) — it never calls
//!   `check_resource_access` on its own `None`-caller_id frame.
//! - The application's HTTP listener is configured with `"flow": "site-main"`
//!   (the native app's `src/serve.rs`), i.e. `DispatchTarget::Flow`, not
//!   `DispatchTarget::Block` — so production never dispatches
//!   `wafer-run/sqlite`/`postgres`/`s3`/crypto/config/network directly as
//!   the top-level target either.
//!
//! This test reconstructs that exact 3-hop shape end-to-end through the
//! REAL `wafer-run/router` block (linked via linkme, not a stand-in) and
//! the REAL `RuntimeContext::check_resource_access`, and asserts the
//! resource-owning block at the far end is ALLOWED — proving the
//! legitimate path is unaffected by Stage 1.
//!
//! No regression found: see `context.rs::cra_denies_anonymous_top_level_caller`
//! for the unit-level pin of the fact this depends on (`None` truly denies
//! — it just never legitimately reaches this check).

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    meta::{META_REQ_ACTION, META_REQ_RESOURCE},
    streams::{input::InputStream, output::OutputStream},
    types::ResourceType,
    Block, BlockInfo,
};
// Pull in the router crate purely for its `register_static_block!` linkme
// side effect — without this `use _` the linker DCEs the crate and
// `wafer-run/router` never reaches `STATIC_BLOCK_REGISTRATIONS`. Must NOT
// call `register_block("wafer-run/router", ...)` manually (DuplicateBlock).
use wafer_block_router as _;
use wafer_run::{Context, StaticConfigSource, Wafer};

/// Stands in for a domain block like `my-org/auth`: invoked by the
/// router, forwards to the resource-owning block below it via its own
/// `ctx.call_block()` — exactly like a real domain block calling
/// `wafer-run/sqlite`.
struct DomainBlock;

#[async_trait]
impl Block for DomainBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test-org/business-block",
            "0.1.0",
            "test/iface@v1",
            "domain block forwarding to its own resource handler",
        )
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        ctx.call_block("test-org/resource-checker", msg, input)
            .await
    }
}

/// Stands in for a resource-owning service handler (database/storage/...):
/// authorizes whoever invoked it via the real
/// `RuntimeContext::check_resource_access`, then reports the verdict in the
/// response body instead of panicking, so the test can assert on the
/// *outcome* of a real dispatch rather than reaching into private fields.
struct ResourceCheckerBlock;

#[async_trait]
impl Block for ResourceCheckerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test-org/resource-checker",
            "0.1.0",
            "test/iface@v1",
            "authorizes its caller against its own namespaced resource",
        )
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        // "test_org__business_block__widgets" is owned by
        // "test-org/business-block" (the DomainBlock above) per the
        // {org}__{block}__{name} convention — mirrors a domain block
        // reading its own table.
        match ctx.check_resource_access(
            "test_org__business_block__widgets",
            ResourceType::Db,
            false,
        ) {
            Ok(()) => OutputStream::respond(b"allowed".to_vec()),
            Err(e) => OutputStream::error(e),
        }
    }
}

#[tokio::test]
async fn legitimate_top_level_router_chain_reaches_allowed_resource_check() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");

    // `wafer-run/router` is already registered via linkme — just configure
    // its routes and register the two downstream test blocks.
    wafer.add_block_config(
        "wafer-run/router",
        serde_json::json!({
            "routes": [
                { "path": "/api/widgets", "actions": ["retrieve"], "block": "test-org/business-block" }
            ]
        }),
    );
    wafer
        .register_block("test-org/business-block", Arc::new(DomainBlock))
        .expect("register domain block");
    wafer
        .register_block("test-org/resource-checker", Arc::new(ResourceCheckerBlock))
        .expect("register resource-checker block");

    wafer.seal().await.expect("seal");
    let wafer = Arc::new(wafer);

    // The exact shape of an inbound HTTP request the listener would hand
    // the router: only req.action/req.resource meta, no WRAP meta, no
    // caller_id (this IS the top-level `caller_id: None` frame).
    let mut msg = Message::new("http.request");
    msg.set_meta(META_REQ_ACTION, "retrieve");
    msg.set_meta(META_REQ_RESOURCE, "/api/widgets");

    // Top-level dispatch — mirrors `wafer-run/http-listener`'s
    // `DispatchTarget::Block` path and (for the flow-configured production
    // case) what each flow step gets via `make_context`: `caller_id: None`.
    let out = wafer
        .run_block("wafer-run/router", msg, InputStream::empty())
        .await;

    match out.collect_buffered().await {
        Ok(buf) => {
            assert_eq!(
                buf.body, b"allowed",
                "the router -> domain-block -> resource-checker chain must \
                 be ALLOWED: caller_id is never None by the time a \
                 resource-access handler runs check_resource_access, \
                 because dispatch_call attributes the callee's caller_id \
                 from the immediate caller's node_id, not its own \
                 (possibly-None) caller_id"
            );
        }
        Err(e) => panic!(
            "REGRESSION: legitimate top-level router->service chain was \
             denied: {e:?} — a real HTTP entry point would 403 on every \
             request"
        ),
    }
}

/// Sanity check on the harness itself: if the resource-checker's *caller*
/// (the domain block) is bypassed and it is invoked directly as the
/// top-level target, THAT is the shape that must legitimately deny (no
/// production route ever does this — see module doc — but it pins the
/// contrast so the ALLOW above isn't accidentally vacuous).
#[tokio::test]
async fn resource_handler_invoked_directly_at_top_level_is_denied() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block("test-org/resource-checker", Arc::new(ResourceCheckerBlock))
        .expect("register resource-checker block");
    wafer.seal().await.expect("seal");
    let wafer = Arc::new(wafer);

    let out = wafer
        .run_block(
            "test-org/resource-checker",
            Message::new("http.request"),
            InputStream::empty(),
        )
        .await;

    match out.collect_buffered().await {
        Err(_) => {} // expected: caller_id is None at this direct top-level frame
        Ok(buf) => panic!(
            "expected denial for a directly-top-level resource handler, got \
             success body {:?} — the harness's ALLOW case above would be \
             vacuous if this were also allowed",
            buf.body
        ),
    }
}
