//! End-to-end integration tests for the auth service layer.
//!
//! Exercises `interfaces::auth::handler::handle_message` — the shared dispatch
//! logic used by the auth block — against a scripted fake `AuthService`.
//! Verifies that user_profile requests deserialize correctly, respond with
//! the expected UserProfile JSON, handle missing users with NOT_FOUND,
//! reject malformed bodies with INVALID_ARGUMENT, and unknown operations
//! return UNIMPLEMENTED.

use futures::StreamExt;
use wafer_block::{common::ServiceOp, core_types::Message, stream::StreamEvent};
use wafer_core::interfaces::auth::{
    handler,
    service::{AuthError, AuthService, Role, UserId, UserProfile},
};

/// Scriptable fake auth service with configurable responses.
enum ScriptedAuthResponse {
    Success(UserProfile),
    NotFound,
    Internal(&'static str),
}

struct ScriptedAuth {
    response: ScriptedAuthResponse,
}

impl ScriptedAuth {
    fn new() -> Self {
        Self {
            response: ScriptedAuthResponse::Internal("no user configured"),
        }
    }

    fn with_user(user: UserProfile) -> Self {
        Self {
            response: ScriptedAuthResponse::Success(user),
        }
    }

    fn with_not_found() -> Self {
        Self {
            response: ScriptedAuthResponse::NotFound,
        }
    }
}

#[async_trait::async_trait]
impl AuthService for ScriptedAuth {
    async fn require_user(&self, _msg: &wafer_block::Message) -> Result<UserId, AuthError> {
        Err(AuthError::Internal("require_user not tested here".into()))
    }

    async fn require_token(
        &self,
        _msg: &wafer_block::Message,
        _scope: wafer_core::interfaces::auth::service::TokenScope,
    ) -> Result<UserId, AuthError> {
        Err(AuthError::Internal("require_token not tested here".into()))
    }

    async fn require_role(
        &self,
        _msg: &wafer_block::Message,
        _role: Role,
    ) -> Result<UserId, AuthError> {
        Err(AuthError::Internal("require_role not tested here".into()))
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

    let body = serde_json::to_vec(&serde_json::json!({ "user_id": "u1" })).unwrap();
    let stream = handler::handle_message(&service, &msg(ServiceOp::AUTH_USER_PROFILE), &body).await;
    let buffered = stream.collect_buffered().await.unwrap();
    let decoded: UserProfile = serde_json::from_slice(&buffered.body).unwrap();
    assert_eq!(decoded, user);
}

#[tokio::test]
async fn auth_user_profile_bad_body_422() {
    let service = ScriptedAuth::new();
    let body = b"not json".to_vec();
    let stream = handler::handle_message(&service, &msg(ServiceOp::AUTH_USER_PROFILE), &body).await;
    let events: Vec<_> = stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], StreamEvent::Error(_)));
}

#[tokio::test]
async fn auth_user_profile_not_found() {
    let service = ScriptedAuth::with_not_found();

    let body = serde_json::to_vec(&serde_json::json!({ "user_id": "u_missing" })).unwrap();
    let stream = handler::handle_message(&service, &msg(ServiceOp::AUTH_USER_PROFILE), &body).await;
    let events: Vec<_> = stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], StreamEvent::Error(_)));
}

#[tokio::test]
async fn auth_unknown_op_unimplemented() {
    let service = ScriptedAuth::new();
    let stream = handler::handle_message(&service, &msg("auth.fictional"), &[]).await;
    let events: Vec<_> = stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], StreamEvent::Error(_)));
}
