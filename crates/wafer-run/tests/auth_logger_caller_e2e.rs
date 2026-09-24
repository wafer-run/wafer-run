//! The auth and logger services know who is calling them, driven end to end:
//! each caller block uses the typed `wafer_core::clients::{auth, logger}`
//! clients through `ctx.call_block` to the REAL `wafer-run/auth` and
//! `wafer-run/logger` blocks, whose handlers read the caller from the REAL
//! `RuntimeContext` and authorize it with `check_resource_access`.
//!
//! The auth block's grants are declared the way an embedder's `AuthService`
//! declares them (`AuthService::grants`, embedded in the block's info), so
//! the runtime's grant registration is on the path too.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceGrant, ResourceType},
    wrap::AUTH_USER_PROFILE_RESOURCE,
    Block, BlockInfo, ErrorCode,
};
use wafer_core::interfaces::{
    auth::service::{AuthError, AuthService, Role, UserId, UserProfile},
    logger::service::{Field, LoggerService},
};
use wafer_run::{Context, Wafer};

/// Holds the auth block's grant to read profiles.
const GRANTED: &str = "acme/directory";
/// Holds no grant: any feature block that authenticates its users.
const FEATURE: &str = "acme/feature";

/// Answers every credential op with one user and `user_profile` with that
/// user's private details, counting profile reads.
struct Auth {
    profile_reads: AtomicUsize,
}

#[async_trait]
impl AuthService for Auth {
    fn grants(&self) -> Vec<ResourceGrant> {
        vec![ResourceGrant::read(GRANTED, AUTH_USER_PROFILE_RESOURCE).typed(ResourceType::Auth)]
    }
    async fn require_user(&self, _msg: &Message) -> Result<UserId, AuthError> {
        Ok(UserId("u1".into()))
    }
    async fn user_profile(&self, user: UserId) -> Result<UserProfile, AuthError> {
        self.profile_reads.fetch_add(1, Ordering::SeqCst);
        Ok(UserProfile {
            id: user,
            email: "victim@example.com".into(),
            display_name: "Victim".into(),
            avatar_url: None,
            role: Role::Admin,
            orgs: Vec::new(),
        })
    }
}

/// Keeps every record as the service received it: caller and message.
#[derive(Default)]
struct Logs(Mutex<Vec<(Option<String>, String)>>);

impl Logs {
    fn push(&self, caller: Option<&str>, msg: &str) {
        self.0
            .lock()
            .unwrap()
            .push((caller.map(str::to_string), msg.to_string()));
    }
}

/// Forwards to the shared record list the test reads.
struct RecordingLogger(Arc<Logs>);

impl LoggerService for RecordingLogger {
    fn debug(&self, caller: Option<&str>, msg: &str, _fields: &[Field]) {
        self.0.push(caller, msg);
    }
    fn info(&self, caller: Option<&str>, msg: &str, _fields: &[Field]) {
        self.0.push(caller, msg);
    }
    fn warn(&self, caller: Option<&str>, msg: &str, _fields: &[Field]) {
        self.0.push(caller, msg);
    }
    fn error(&self, caller: Option<&str>, msg: &str, _fields: &[Field]) {
        self.0.push(caller, msg);
    }
}

/// A caller block: `profile` reads user `u1`'s profile, `whoami` resolves
/// the credential on its request, `log` logs its request's `note` meta —
/// each through the typed client, from the block's own context.
struct Caller {
    name: &'static str,
}

#[async_trait]
impl Block for Caller {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "calls auth and logger")
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        match msg.kind.as_str() {
            "profile" => match wafer_core::clients::auth::user_profile(ctx, "u1".into()).await {
                Ok(p) => OutputStream::respond(p.email.into_bytes()),
                Err(e) => OutputStream::error(e),
            },
            "whoami" => match wafer_core::clients::auth::require_user(ctx, &msg).await {
                Ok(id) => OutputStream::respond(id.into_bytes()),
                Err(e) => OutputStream::error(e),
            },
            "log" => {
                wafer_core::clients::logger::info(ctx, msg.get_meta("note")).await;
                OutputStream::respond(Vec::new())
            }
            other => OutputStream::error(WaferError::new(ErrorCode::Unimplemented, other)),
        }
    }
}

async fn build() -> (Arc<Wafer>, Arc<Auth>, Arc<Logs>) {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    let auth = Arc::new(Auth {
        profile_reads: AtomicUsize::new(0),
    });
    let logs = Arc::new(Logs::default());
    wafer_core::service_blocks::auth::register_with(&mut wafer, auth.clone())
        .expect("register wafer-run/auth");
    wafer_core::service_blocks::logger::register_with(
        &mut wafer,
        Arc::new(RecordingLogger(logs.clone())),
    )
    .expect("register wafer-run/logger");
    for name in [GRANTED, FEATURE] {
        wafer
            .register_block(name, Arc::new(Caller { name }))
            .expect("register caller");
    }
    wafer.seal().await.expect("seal");
    (Arc::new(wafer), auth, logs)
}

/// Run `kind` as `caller`: the response body, or the code it failed with.
async fn run(wafer: &Wafer, caller: &str, msg: Message) -> Result<Vec<u8>, ErrorCode> {
    let out = wafer
        .run_block(caller, msg, InputStream::from_bytes(Vec::new()))
        .await;
    match out.collect_buffered().await {
        Ok(resp) => Ok(resp.body),
        Err(TerminalNotResponse::Error(e)) => Err(e.code),
        Err(other) => panic!("no response or error: {other:?}"),
    }
}

/// The finding: any block that could reach `wafer-run/auth` read any user's
/// email, role and orgs through `user_profile`. A block the auth block did
/// not grant is now refused, and the service never runs.
#[tokio::test]
async fn user_profile_is_refused_to_a_block_without_the_auth_blocks_grant() {
    let (wafer, auth, _) = build().await;

    assert_eq!(
        run(&wafer, FEATURE, Message::new("profile")).await,
        Err(ErrorCode::PermissionDenied)
    );
    assert_eq!(auth.profile_reads.load(Ordering::SeqCst), 0);
}

/// The auth block's grant admits its grantee — the runtime registers a typed
/// `Auth` grant the auth block declares on its own namespace.
#[tokio::test]
async fn user_profile_is_served_to_the_block_the_auth_block_granted() {
    let (wafer, auth, _) = build().await;

    assert_eq!(
        run(&wafer, GRANTED, Message::new("profile")).await,
        Ok(b"victim@example.com".to_vec())
    );
    assert_eq!(auth.profile_reads.load(Ordering::SeqCst), 1);
}

/// The credential ops stay open to every block: the answer comes from the
/// credential the block forwards, not from the auth block's authority.
#[tokio::test]
async fn require_user_needs_no_grant() {
    let (wafer, _, _) = build().await;

    let mut msg = Message::new("whoami");
    msg.set_meta("http.header.authorization", "Bearer t");
    assert_eq!(run(&wafer, FEATURE, msg).await, Ok(b"u1".to_vec()));
}

/// The logger service receives the caller the runtime registered — so a
/// line is attributable — and the block's newline escaped, so one call is
/// one record.
#[tokio::test]
async fn a_log_line_carries_its_registered_caller_on_one_line() {
    let (wafer, _, logs) = build().await;

    let mut msg = Message::new("log");
    msg.set_meta("note", "done\nERROR acme/directory: forged");
    run(&wafer, FEATURE, msg).await.expect("log");

    assert_eq!(
        *logs.0.lock().unwrap(),
        vec![(
            Some(FEATURE.to_string()),
            "done\\nERROR acme/directory: forged".to_string()
        )]
    );
}
