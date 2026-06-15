//! AuthService cross-block contract — implemented by suppers-ai/auth.
//!
//! See docs/superpowers/specs/2026-04-21-auth-block-design.md §4.

use thiserror::Error;
use wafer_block::Message;
use wafer_block_macro::wafer_async_trait;

/// Opaque user identifier. Wraps a uuid v7 string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct UserId(pub String);

/// Coarse-grained role attached to a user account.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    /// Regular user without admin privileges.
    User,
    /// Administrator with elevated privileges.
    Admin,
}

/// Scope a Personal Access Token must carry to authorize an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScope {
    /// Permits publishing block releases / artifacts on the user's behalf.
    Publish,
}

/// Summary view of an organization the user belongs to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OrgSummary {
    /// Organization name (matches the `{org}` slug used in block ids).
    pub name: String,
    /// Identity provider used to verify ownership (e.g. `github`), if any.
    pub verified_via: Option<String>,
    /// Provider-specific reference (e.g. GitHub org slug) recorded at verification time.
    pub verified_ref: Option<String>,
    /// Whether this org is reserved (only admins may operate on reserved orgs).
    pub is_reserved: bool,
}

/// Full user profile returned by [`AuthService::user_profile`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UserProfile {
    /// Stable user identifier.
    pub id: UserId,
    /// Primary email address.
    pub email: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Optional avatar URL.
    pub avatar_url: Option<String>,
    /// Role assigned to the user.
    pub role: Role,
    /// Organizations the user is a member of.
    pub orgs: Vec<OrgSummary>,
}

/// Errors returned by [`AuthService`] operations.
#[derive(Error, Debug)]
pub enum AuthError {
    /// No valid credential present on the request.
    #[error("unauthorized")]
    Unauthorized,
    /// Credentials are valid but the action is not permitted.
    #[error("forbidden")]
    Forbidden,
    /// Upstream identity provider returned an error or was unreachable.
    #[error("provider down: {0}")]
    ProviderDown(String),
    /// Requested user / resource does not exist.
    #[error("not found")]
    NotFound,
    /// Catch-all internal failure.
    #[error("{0}")]
    Internal(String),
}

/// Cross-block auth contract. See spec §4.
#[wafer_async_trait]
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
    ///
    /// Fail-closed default: a service that does not implement authentication
    /// rejects every request as unauthorized.
    async fn require_user(&self, _msg: &Message) -> Result<UserId, AuthError> {
        Err(AuthError::Unauthorized)
    }

    /// As require_user but requires a PAT with the given scope.
    /// Session cookies are rejected — scopes only apply to PATs.
    ///
    /// Fail-closed default: unauthorized.
    async fn require_token(&self, _msg: &Message, _scope: TokenScope) -> Result<UserId, AuthError> {
        Err(AuthError::Unauthorized)
    }

    /// require_user + users.role == role. Also accepts a valid unexpired
    /// bootstrap_tokens row as Admin.
    ///
    /// Fail-closed default: unauthorized.
    async fn require_role(&self, _msg: &Message, _role: Role) -> Result<UserId, AuthError> {
        Err(AuthError::Unauthorized)
    }

    /// True iff user owns the org. Reserved orgs return true iff user.role == Admin.
    /// Non-github providers return Ok(false). (Implemented in Plan C.)
    ///
    /// Fail-closed default: not an org admin.
    async fn verify_org_admin(
        &self,
        _user: UserId,
        _provider: &str,
        _org_ref: &str,
    ) -> Result<bool, AuthError> {
        Ok(false)
    }

    /// Look up the full profile for `user`.
    ///
    /// Fail-closed default: no profile.
    async fn user_profile(&self, _user: UserId) -> Result<UserProfile, AuthError> {
        Err(AuthError::NotFound)
    }
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
