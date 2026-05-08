//! AuthService cross-block contract — implemented by suppers-ai/auth.
//!
//! See docs/superpowers/specs/2026-04-21-auth-block-design.md §4.

use thiserror::Error;
use wafer_block::Message;

/// Opaque user identifier. Wraps a uuid v7 string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct UserId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    User,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScope {
    Publish,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OrgSummary {
    pub name: String,
    pub verified_via: Option<String>,
    pub verified_ref: Option<String>,
    pub is_reserved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UserProfile {
    pub id: UserId,
    pub email: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub role: Role,
    pub orgs: Vec<OrgSummary>,
}

#[derive(Error, Debug)]
pub enum AuthError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("provider down: {0}")]
    ProviderDown(String),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Internal(String),
}

/// Cross-block auth contract. See spec §4.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait AuthService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// One-shot lifecycle hook. Invoked by the framework `AuthBlock` on
    /// `LifecycleType::Init`. Default implementation is a no-op; concrete
    /// implementations override this to apply migrations, run bootstrap, etc.
    async fn init(&self, _ctx: &dyn wafer_block::context::Context) -> Result<(), AuthError> {
        Ok(())
    }

    /// WRAP resource grants this auth block declares for other blocks.
    ///
    /// Default empty; concrete services override to declare grants for
    /// consumer blocks (e.g. `suppers-ai/auth-ui`, `admin`, `userportal`).
    /// The framework `AuthBlock::info()` embeds the result of this method
    /// into `BlockInfo::grants` so the runtime registers the grants at
    /// startup.
    fn grants(&self) -> Vec<wafer_block::types::ResourceGrant> {
        Vec::new()
    }

    /// Extract credentials from incoming Message (Bearer or Cookie).
    /// Look up in sessions or personal_access_tokens; touch last_used_at.
    async fn require_user(&self, msg: &Message) -> Result<UserId, AuthError>;

    /// As require_user but requires a PAT with the given scope.
    /// Session cookies are rejected — scopes only apply to PATs.
    async fn require_token(&self, msg: &Message, scope: TokenScope) -> Result<UserId, AuthError>;

    /// require_user + users.role == role. Also accepts a valid unexpired
    /// bootstrap_tokens row as Admin.
    async fn require_role(&self, msg: &Message, role: Role) -> Result<UserId, AuthError>;

    /// True iff user owns the org. Reserved orgs return true iff user.role == Admin.
    /// Non-github providers return Ok(false). (Implemented in Plan C.)
    async fn verify_org_admin(
        &self,
        user: UserId,
        provider: &str,
        org_ref: &str,
    ) -> Result<bool, AuthError>;

    async fn user_profile(&self, user: UserId) -> Result<UserProfile, AuthError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-only: ensure the trait is object-safe and the error/types round-trip.
    #[test]
    fn trait_is_object_safe() {
        fn _takes_dyn(_x: &dyn AuthService) {}
        let id = UserId("u1".to_string());
        assert_eq!(id, UserId("u1".to_string()));
        assert!(matches!(Role::Admin, Role::Admin));
        assert!(matches!(TokenScope::Publish, TokenScope::Publish));
        let err = AuthError::Unauthorized;
        assert_eq!(format!("{err}"), "unauthorized");
    }
}
