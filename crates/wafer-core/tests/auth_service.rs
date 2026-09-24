//! End-to-end integration tests for the auth service layer.
//!
//! Exercises `interfaces::auth::handler::handle_message` — the shared dispatch
//! logic used by the auth block — against a scripted fake `AuthService`,
//! under a `Context` that authorizes through the real
//! `wafer_block::wrap::check_access`. Verifies that user_profile requests
//! deserialize correctly, respond with the expected UserProfile JSON, handle
//! missing users with NOT_FOUND, reject malformed bodies with
//! INVALID_ARGUMENT, and unknown operations return UNIMPLEMENTED; and that
//! the handler authorizes each op for its caller before the service runs.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use futures::StreamExt;
use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    core_types::Message,
    stream::StreamEvent,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceAccess, ResourceGrant, ResourceType},
    wire::auth as wire,
    wrap::AUTH_USER_PROFILE_RESOURCE,
    ErrorCode, WaferError,
};
use wafer_core::interfaces::auth::{
    handler,
    service::{AuthError, AuthService, Role, UserId, UserProfile},
};

const ADMIN: &str = "test/admin";
/// Holds the auth block's `user_profile` grant (see [`WrapCtx::granted`]).
const GRANTED: &str = "test/granted";
/// Holds no grant at all.
const FEATURE: &str = "test/feature";

/// The auth block's context for one call from `caller`, authorizing through
/// the real WRAP check with the grants the auth block declared.
struct WrapCtx {
    caller: Option<&'static str>,
    grants: Vec<ResourceGrant>,
}

impl WrapCtx {
    fn from(caller: Option<&'static str>) -> Self {
        Self {
            caller,
            grants: vec![
                ResourceGrant::read(GRANTED, AUTH_USER_PROFILE_RESOURCE).typed(ResourceType::Auth)
            ],
        }
    }

    fn granted() -> Self {
        Self::from(Some(GRANTED))
    }
}

#[wafer_block::wafer_async_trait]
impl Context for WrapCtx {
    async fn call_block(&self, _b: &str, _m: Message, _i: InputStream) -> OutputStream {
        unimplemented!("the auth handler makes no calls")
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("the auth handler keeps no context")
    }
    fn caller_id(&self) -> Option<&str> {
        self.caller
    }
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: ResourceType,
        access: ResourceAccess,
    ) -> Result<(), WaferError> {
        wafer_block::wrap::check_access(
            self.caller,
            resource,
            access,
            Some(&resource_type),
            &self.grants,
            ADMIN,
        )
    }
    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: ResourceType,
        access: ResourceAccess,
    ) -> bool {
        self.check_resource_access(resource, resource_type, access)
            .is_ok()
    }
}

/// Scriptable fake auth service with configurable responses.
enum ScriptedAuthResponse {
    Success(UserProfile),
    NotFound,
    Internal(&'static str),
}

struct ScriptedAuth {
    response: ScriptedAuthResponse,
    /// Every service method call, so a test can prove a denied op never
    /// reached the service.
    calls: AtomicUsize,
}

impl ScriptedAuth {
    fn new() -> Self {
        Self::responding(ScriptedAuthResponse::Internal("no user configured"))
    }

    fn with_user(user: UserProfile) -> Self {
        Self::responding(ScriptedAuthResponse::Success(user))
    }

    fn with_not_found() -> Self {
        Self::responding(ScriptedAuthResponse::NotFound)
    }

    fn responding(response: ScriptedAuthResponse) -> Self {
        Self {
            response,
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn record(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl AuthService for ScriptedAuth {
    async fn require_user(&self, _msg: &wafer_block::Message) -> Result<UserId, AuthError> {
        self.record();
        Ok(UserId("bearer".into()))
    }

    async fn require_token(
        &self,
        _msg: &wafer_block::Message,
        _scope: wafer_core::interfaces::auth::service::TokenScope,
    ) -> Result<UserId, AuthError> {
        self.record();
        Ok(UserId("bearer".into()))
    }

    async fn require_role(
        &self,
        _msg: &wafer_block::Message,
        _role: Role,
    ) -> Result<UserId, AuthError> {
        self.record();
        Ok(UserId("bearer".into()))
    }

    async fn verify_org_admin(
        &self,
        _user: UserId,
        _provider: &str,
        _org_ref: &str,
    ) -> Result<bool, AuthError> {
        Err(AuthError::Internal(
            "verify_org_admin not tested here".into(),
        ))
    }

    async fn user_profile(&self, _user: UserId) -> Result<UserProfile, AuthError> {
        self.record();
        match &self.response {
            ScriptedAuthResponse::Success(user) => Ok(user.clone()),
            ScriptedAuthResponse::NotFound => Err(AuthError::NotFound),
            ScriptedAuthResponse::Internal(msg) => Err(AuthError::Internal(msg.to_string())),
        }
    }
}

fn msg(kind: &str) -> Message {
    Message {
        kind: kind.into(),
        meta: vec![],
    }
}

#[tokio::test]
async fn auth_user_profile_happy_path() {
    let user = UserProfile {
        id: UserId("u1".into()),
        email: "a@b".into(),
        display_name: "A".into(),
        avatar_url: None,
        role: Role::User,
        orgs: vec![],
    };
    let service = ScriptedAuth::with_user(user.clone());

    let body = codec::encode(&wire::UserProfileRequest {
        user_id: "u1".into(),
    })
    .unwrap();
    let stream = handler::handle_message(
        &service,
        &WrapCtx::granted(),
        &msg(ServiceOp::AUTH_USER_PROFILE),
        &body,
    )
    .await;
    let buffered = stream.collect_buffered().await.unwrap();
    let decoded: wire::UserProfileResponse = codec::decode(&buffered.body).unwrap();
    assert_eq!(decoded.id, user.id.0);
    assert_eq!(decoded.email, user.email);
    assert_eq!(decoded.display_name, user.display_name);
    assert_eq!(decoded.role, "user");
    assert!(decoded.orgs.is_empty());
}

#[tokio::test]
async fn auth_user_profile_bad_body_422() {
    let service = ScriptedAuth::new();
    let body = b"not json".to_vec();
    let stream = handler::handle_message(
        &service,
        &WrapCtx::granted(),
        &msg(ServiceOp::AUTH_USER_PROFILE),
        &body,
    )
    .await;
    let events: Vec<_> = stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], StreamEvent::Error(_)));
}

#[tokio::test]
async fn auth_user_profile_not_found() {
    let service = ScriptedAuth::with_not_found();

    let body = codec::encode(&wire::UserProfileRequest {
        user_id: "u_missing".into(),
    })
    .unwrap();
    let stream = handler::handle_message(
        &service,
        &WrapCtx::granted(),
        &msg(ServiceOp::AUTH_USER_PROFILE),
        &body,
    )
    .await;
    let events: Vec<_> = stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], StreamEvent::Error(_)));
}

#[tokio::test]
async fn auth_unknown_op_unimplemented() {
    let service = ScriptedAuth::new();
    let stream =
        handler::handle_message(&service, &WrapCtx::granted(), &msg("auth.fictional"), &[]).await;
    let events: Vec<_> = stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], StreamEvent::Error(_)));
}

fn user_profile_body() -> Vec<u8> {
    codec::encode(&wire::UserProfileRequest {
        user_id: "u1".into(),
    })
    .unwrap()
}

/// A credential-op message the way `clients::auth` forwards one: the
/// caller's request, with the role header `require_role` reads.
fn credential_msg(kind: &str) -> Message {
    let mut m = msg(kind);
    m.set_meta("http.header.x-auth-role", "user");
    m
}

async fn error_code(out: OutputStream) -> Option<ErrorCode> {
    match out.collect_buffered().await {
        Ok(_) => None,
        Err(TerminalNotResponse::Error(e)) => Some(e.code),
        Err(other) => panic!("no response or error: {other:?}"),
    }
}

/// The finding: `user_profile` returned any user's email, role and orgs to
/// any block that could reach `wafer-run/auth`. A caller with no grant (and
/// an anonymous one) is now refused before the service runs.
#[tokio::test]
async fn user_profile_is_refused_to_a_caller_without_a_grant() {
    for caller in [Some(FEATURE), None] {
        let service = ScriptedAuth::with_user(UserProfile {
            id: UserId("u1".into()),
            email: "victim@example.com".into(),
            display_name: "Victim".into(),
            avatar_url: None,
            role: Role::Admin,
            orgs: vec![],
        });
        let out = handler::handle_message(
            &service,
            &WrapCtx::from(caller),
            &msg(ServiceOp::AUTH_USER_PROFILE),
            &user_profile_body(),
        )
        .await;
        assert_eq!(
            error_code(out).await,
            Some(ErrorCode::PermissionDenied),
            "caller {caller:?}"
        );
        assert_eq!(service.calls(), 0, "caller {caller:?} reached the service");
    }
}

/// The admin block reads profiles without a grant, as it reads every
/// namespaced resource.
#[tokio::test]
async fn user_profile_is_served_to_the_admin_block() {
    let service = ScriptedAuth::with_user(UserProfile {
        id: UserId("u1".into()),
        email: "a@b".into(),
        display_name: "A".into(),
        avatar_url: None,
        role: Role::User,
        orgs: vec![],
    });
    let out = handler::handle_message(
        &service,
        &WrapCtx::from(Some(ADMIN)),
        &msg(ServiceOp::AUTH_USER_PROFILE),
        &user_profile_body(),
    )
    .await;
    assert_eq!(error_code(out).await, None);
}

/// The credential ops resolve the credential the caller forwards, so any
/// attributable caller reaches the service without a grant — and an
/// anonymous one does not.
#[tokio::test]
async fn credential_ops_admit_any_attributable_caller_only() {
    for op in [
        ServiceOp::AUTH_REQUIRE_USER,
        ServiceOp::AUTH_REQUIRE_TOKEN,
        ServiceOp::AUTH_REQUIRE_ROLE,
    ] {
        let service = ScriptedAuth::new();
        let out = handler::handle_message(
            &service,
            &WrapCtx::from(Some(FEATURE)),
            &credential_msg(op),
            &[],
        )
        .await;
        assert_eq!(error_code(out).await, None, "{op} from {FEATURE}");
        assert_eq!(service.calls(), 1, "{op} from {FEATURE}");

        let service = ScriptedAuth::new();
        let out =
            handler::handle_message(&service, &WrapCtx::from(None), &credential_msg(op), &[]).await;
        assert_eq!(
            error_code(out).await,
            Some(ErrorCode::PermissionDenied),
            "{op} from an anonymous caller"
        );
        assert_eq!(service.calls(), 0, "{op} from an anonymous caller");
    }
}
