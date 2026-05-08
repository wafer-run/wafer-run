//! Typed client for the unified auth block (`suppers-ai/auth`).
//!
//! Mirrors `clients::crypto` shape. Three of the four ops forward the
//! caller's incoming `Message` (so the auth block can read Bearer / Cookie
//! headers from the original request) — these use `svc_msg!` instead of
//! `svc!`. The fourth (`user_profile`) is a plain typed body call.
//!
//! Header injection for `require_role` / `require_token` follows the
//! convention in `interfaces::auth::handler`: scope is read from
//! `x-auth-scope`, role from `x-auth-role`. Both are stored as ordinary HTTP
//! request headers under `http.header.<name>`.

#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
use wafer_block::{
    common::ServiceOp,
    wire::auth::{UserIdResponse, UserProfileRequest, UserProfileResponse},
    Message, WaferError,
};

use super::{call_service, call_service_with_msg, decode, dual_api, svc, svc_msg};

const BLOCK: &str = "suppers-ai/auth";

/// Clone `msg`, override `kind`, and return the new message ready to forward.
///
/// `META_REQ_ACTION` is set by `call_service_with_msg`, so we only override
/// `kind` here — caller-supplied request meta (Authorization, Cookie, etc.)
/// is preserved on the clone.
fn forward_msg(msg: &Message, kind: &str) -> Message {
    let mut out = msg.clone();
    out.kind = kind.to_string();
    out
}

// ===========================================================================
// Public API — generated as async (native) or sync (wasm-component)
// ===========================================================================

dual_api! {
    /// Resolve the caller's `UserId` from the incoming `Message`.
    ///
    /// The auth block reads Bearer / Cookie credentials from the forwarded
    /// message meta and looks them up in sessions / personal_access_tokens.
    /// Returns `Err(WaferError { code: UNAUTHENTICATED, .. })` when no valid
    /// credential is present.
    pub fn require_user(ctx, msg: &Message) -> Result<String, WaferError> {
        let fwd = forward_msg(msg, ServiceOp::AUTH_REQUIRE_USER);
        let data = svc_msg!(ctx, BLOCK, fwd, None, false, Some("auth"))?;
        let resp: UserIdResponse = decode(&data)?;
        Ok(resp.user_id)
    }

    /// Like `require_user`, but additionally requires the caller's role to
    /// match `role` (case-insensitive: `"admin"` → `Role::Admin`, anything
    /// else → `Role::User`). Role is forwarded via the `x-auth-role` header.
    pub fn require_role(ctx, msg: &Message, role: &str) -> Result<String, WaferError> {
        let mut fwd = forward_msg(msg, ServiceOp::AUTH_REQUIRE_ROLE);
        fwd.set_meta("http.header.x-auth-role", role);
        let data = svc_msg!(ctx, BLOCK, fwd, None, false, Some("auth"))?;
        let resp: UserIdResponse = decode(&data)?;
        Ok(resp.user_id)
    }

    /// Like `require_user`, but requires a Personal Access Token bearing
    /// the given scope. Session cookies are rejected — scopes apply to PATs
    /// only. Scope is forwarded via the `x-auth-scope` header.
    pub fn require_token(ctx, msg: &Message, scope: &str) -> Result<String, WaferError> {
        let mut fwd = forward_msg(msg, ServiceOp::AUTH_REQUIRE_TOKEN);
        fwd.set_meta("http.header.x-auth-scope", scope);
        let data = svc_msg!(ctx, BLOCK, fwd, None, false, Some("auth"))?;
        let resp: UserIdResponse = decode(&data)?;
        Ok(resp.user_id)
    }

    /// Fetch the profile for `user_id`. Does not consult the caller's
    /// request meta — `user_id` is supplied directly in the body.
    pub fn user_profile(ctx, user_id: String) -> Result<UserProfileResponse, WaferError> {
        let req = UserProfileRequest { user_id };
        let data = svc!(
            ctx, BLOCK,
            ServiceOp::AUTH_USER_PROFILE,
            &req,
            None,
            false,
            Some("auth")
        )?;
        let resp: UserProfileResponse = decode(&data)?;
        Ok(resp)
    }
}

#[cfg(all(test, not(feature = "wasm-component")))]
mod tests {
    use std::sync::Arc;

    use wafer_block::{
        block::Block,
        context::Context,
        streams::{input::InputStream, output::OutputStream},
        Message,
    };

    use crate::{
        interfaces::auth::service::{
            AuthError, AuthService, Role, TokenScope, UserId, UserProfile,
        },
        service_blocks::auth::AuthBlock,
    };

    /// Stub `AuthService` that returns `UserId("u-1")` for all auth checks
    /// and a canned profile for `user_profile`.
    struct HappyAuth;

    #[async_trait::async_trait]
    impl AuthService for HappyAuth {
        async fn require_user(&self, _msg: &Message) -> Result<UserId, AuthError> {
            Ok(UserId("u-1".to_string()))
        }
        async fn require_token(
            &self,
            _msg: &Message,
            _scope: TokenScope,
        ) -> Result<UserId, AuthError> {
            Ok(UserId("u-1".to_string()))
        }
        async fn require_role(&self, _msg: &Message, _role: Role) -> Result<UserId, AuthError> {
            Ok(UserId("u-1".to_string()))
        }
        async fn verify_org_admin(
            &self,
            _user: UserId,
            _provider: &str,
            _org_ref: &str,
        ) -> Result<bool, AuthError> {
            Ok(false)
        }
        async fn user_profile(&self, user: UserId) -> Result<UserProfile, AuthError> {
            Ok(UserProfile {
                id: user,
                email: "alice@example.test".to_string(),
                display_name: "Alice".to_string(),
                avatar_url: None,
                role: Role::User,
                orgs: Vec::new(),
            })
        }
    }

    /// Single-block test context that routes every `call_block` to the
    /// wrapped `AuthBlock`. wafer-core has no full-runtime infrastructure;
    /// this tiny stub gives the typed-client tests a real round-trip
    /// without pulling in `wafer-run`.
    struct SingleAuthBlockCtx {
        block: Arc<AuthBlock>,
    }

    #[async_trait::async_trait]
    impl Context for SingleAuthBlockCtx {
        async fn call_block(
            &self,
            block_name: &str,
            msg: Message,
            input: InputStream,
        ) -> OutputStream {
            assert_eq!(block_name, "suppers-ai/auth");
            self.block.handle(self, msg, input).await
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }
    }

    fn happy_ctx() -> SingleAuthBlockCtx {
        SingleAuthBlockCtx {
            block: Arc::new(AuthBlock::new(Arc::new(HappyAuth))),
        }
    }

    #[tokio::test]
    async fn require_user_happy_path_round_trips() {
        let ctx = happy_ctx();
        let msg = Message::new("incoming.req");
        let user_id = super::require_user(&ctx, &msg).await.expect("require_user");
        assert_eq!(user_id, "u-1");
    }

    #[tokio::test]
    async fn require_role_forwards_header_and_round_trips() {
        let ctx = happy_ctx();
        let msg = Message::new("incoming.req");
        let user_id = super::require_role(&ctx, &msg, "admin")
            .await
            .expect("require_role");
        assert_eq!(user_id, "u-1");
    }

    #[tokio::test]
    async fn require_token_forwards_scope_and_round_trips() {
        let ctx = happy_ctx();
        let msg = Message::new("incoming.req");
        let user_id = super::require_token(&ctx, &msg, "publish")
            .await
            .expect("require_token");
        assert_eq!(user_id, "u-1");
    }

    #[tokio::test]
    async fn user_profile_round_trips() {
        let ctx = happy_ctx();
        let profile = super::user_profile(&ctx, "u-1".to_string())
            .await
            .expect("user_profile");
        assert_eq!(profile.id, "u-1");
        assert_eq!(profile.email, "alice@example.test");
        assert_eq!(profile.role, "User");
    }

    #[test]
    fn forward_msg_overrides_kind_and_preserves_meta() {
        let mut original = Message::new("incoming.req");
        original.set_meta("http.header.authorization", "Bearer abc");
        original.set_meta("http.header.cookie", "sb_session=xyz");

        let fwd = super::forward_msg(&original, "auth.require_user");
        assert_eq!(fwd.kind, "auth.require_user");
        assert_eq!(fwd.get_meta("http.header.authorization"), "Bearer abc");
        assert_eq!(fwd.get_meta("http.header.cookie"), "sb_session=xyz");
    }
}
