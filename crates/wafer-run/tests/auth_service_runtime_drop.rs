//! A runtime with an `AuthService` is freed when the `Wafer` is dropped.
//!
//! `AuthService` hands every method the `ctx` of its call, so a service that
//! dispatches (here: `require_user` logs through the logger block) keeps
//! nothing of the runtime. The probe is a `Weak` on the registered
//! `wafer-run/auth` block: the runtime's context owns every registered block,
//! so anything that keeps a context alive keeps that block alive too.
//!
//! The second test is the rule `Context::clone_arc` states, shown on the
//! runtime: a service that stores the `init` context closes a cycle through
//! the registered blocks, and the runtime outlives its `Wafer` (that test
//! leaks its runtime for the rest of the test process, by design).

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    Block, BlockInfo,
};
use wafer_core::interfaces::{
    auth::service::{AuthError, AuthService, UserId},
    logger::service::{Field, LoggerService},
};
use wafer_run::{Context, Wafer};

const CALLER: &str = "acme/feature";

/// Resolves every credential to one user, logging through the `ctx` of the
/// call: it reaches another block and keeps nothing.
struct PerCallAuth;

#[async_trait]
impl AuthService for PerCallAuth {
    async fn require_user(&self, ctx: &dyn Context, _msg: &Message) -> Result<UserId, AuthError> {
        wafer_core::clients::logger::info(ctx, "resolved").await;
        Ok(UserId("u1".into()))
    }
}

/// Stores the `init` context, the shape `Context::clone_arc` forbids.
struct StashingAuth {
    ctx: OnceLock<Arc<dyn Context>>,
}

#[async_trait]
impl AuthService for StashingAuth {
    async fn init(&self, ctx: &dyn Context) -> Result<(), AuthError> {
        let _ = self.ctx.set(ctx.clone_arc());
        Ok(())
    }
    async fn require_user(&self, _ctx: &dyn Context, _msg: &Message) -> Result<UserId, AuthError> {
        Ok(UserId("u1".into()))
    }
}

struct NullLogger;

impl LoggerService for NullLogger {
    fn debug(&self, _caller: Option<&str>, _msg: &str, _fields: &[Field]) {}
    fn info(&self, _caller: Option<&str>, _msg: &str, _fields: &[Field]) {}
    fn warn(&self, _caller: Option<&str>, _msg: &str, _fields: &[Field]) {}
    fn error(&self, _caller: Option<&str>, _msg: &str, _fields: &[Field]) {}
}

/// Resolves the credential on its request through the typed client.
struct Caller;

#[async_trait]
impl Block for Caller {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(CALLER, "0.1.0", "test/iface@v1", "authenticates its user")
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        match wafer_core::clients::auth::require_user(ctx, &msg).await {
            Ok(id) => OutputStream::respond(id.into_bytes()),
            Err(e) => OutputStream::error(e),
        }
    }
}

/// Builds a sealed runtime around `auth`, initializes the auth block through
/// a real dispatch, and returns a `Weak` on the registered auth block.
async fn run_then_probe(auth: Arc<dyn AuthService>) -> Weak<dyn Block> {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    wafer_core::service_blocks::auth::register_with(&mut wafer, auth)
        .expect("register wafer-run/auth");
    wafer_core::service_blocks::logger::register_with(&mut wafer, Arc::new(NullLogger))
        .expect("register wafer-run/logger");
    wafer
        .register_block(CALLER, Arc::new(Caller))
        .expect("register caller");
    wafer.seal().await.expect("seal");

    let out = wafer
        .run_block(
            CALLER,
            Message::new("whoami"),
            InputStream::from_bytes(Vec::new()),
        )
        .await;
    match out.collect_buffered().await {
        Ok(resp) => assert_eq!(resp.body, b"u1"),
        Err(TerminalNotResponse::Error(e)) => panic!("whoami failed: {e:?}"),
        Err(other) => panic!("no response or error: {other:?}"),
    }

    let probe = Arc::downgrade(&wafer.lookup_block("wafer-run/auth").expect("auth").1);
    drop(wafer);
    probe
}

#[tokio::test]
async fn a_runtime_whose_auth_service_takes_ctx_per_call_is_freed_on_drop() {
    let probe = run_then_probe(Arc::new(PerCallAuth)).await;
    assert!(
        probe.upgrade().is_none(),
        "the auth block outlived its Wafer: something keeps the runtime's context"
    );
}

#[tokio::test]
async fn a_service_that_stores_its_init_context_keeps_the_runtime_alive() {
    let probe = run_then_probe(Arc::new(StashingAuth {
        ctx: OnceLock::new(),
    }))
    .await;
    assert!(
        probe.upgrade().is_some(),
        "a stored init context is a cycle through the registered blocks"
    );
}
