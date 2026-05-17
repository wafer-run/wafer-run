//! Wire-format types for the auth service.
//!
//! Mirrors `crates/wafer-core/src/interfaces/auth/handler.rs`. The handler
//! types `UserProfileRequest` and `UserIdResponse` are private to the handler;
//! this module exposes equivalents under the canonical wire-types location.
//!
//! `UserProfile`, `OrgSummary`, `UserId`, and `Role` are defined in the
//! `auth::service` module and already implement `Serialize`/`Deserialize`
//! there. Wire shapes here intentionally use plain types (`String` user ids,
//! `String` role) rather than re-importing the service types so the
//! shared-types crate stays free of dependencies on `wafer-core`.

use serde::{Deserialize, Serialize};

// --- Requests ---

/// Request for `auth.user_profile`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfileRequest {
    /// User id whose profile to fetch.
    pub user_id: String,
}

// --- Responses ---

/// Response for `auth.require_user` / `auth.require_token` / `auth.require_role`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserIdResponse {
    /// Authenticated user id resolved from the request context.
    pub user_id: String,
}

/// Org summary embedded in `UserProfileResponse`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgSummary {
    /// Org short name (slug).
    pub name: String,
    /// External provider used for org verification (e.g. `"github"`).
    pub verified_via: Option<String>,
    /// External provider-side identifier the verification refers to.
    pub verified_ref: Option<String>,
    /// Whether this org name is reserved by the platform.
    pub is_reserved: bool,
}

/// Response for `auth.user_profile`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfileResponse {
    /// User id.
    pub id: String,
    /// Primary email address.
    pub email: String,
    /// User-chosen display name.
    pub display_name: String,
    /// Optional avatar URL.
    pub avatar_url: Option<String>,
    /// `"User"` or `"Admin"` — matches `auth::service::Role` serde repr.
    pub role: String,
    /// Orgs this user is a member of.
    pub orgs: Vec<OrgSummary>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    #[test]
    fn user_profile_request_round_trips() {
        let original = UserProfileRequest {
            user_id: "u-123".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: UserProfileRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.user_id, original.user_id);
    }

    #[test]
    fn user_id_response_round_trips() {
        let original = UserIdResponse {
            user_id: "u-1".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: UserIdResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.user_id, original.user_id);
    }

    #[test]
    fn user_profile_response_round_trips() {
        let original = UserProfileResponse {
            id: "u-1".into(),
            email: "a@b.test".into(),
            display_name: "Alice".into(),
            avatar_url: Some("https://example.test/a.png".into()),
            role: "User".into(),
            orgs: vec![OrgSummary {
                name: "acme".into(),
                verified_via: Some("github".into()),
                verified_ref: Some("123".into()),
                is_reserved: false,
            }],
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: UserProfileResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.id, original.id);
        assert_eq!(decoded.email, original.email);
        assert_eq!(decoded.role, original.role);
        assert_eq!(decoded.orgs.len(), 1);
        assert_eq!(decoded.orgs[0].name, "acme");
    }

    #[test]
    fn schema_lock_user_profile_request() {
        let req = UserProfileRequest {
            user_id: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a7757365725f6964a0",
            "UserProfileRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_user_id_response() {
        let resp = UserIdResponse {
            user_id: String::new(),
        };
        let encoded = codec::encode(&resp).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a7757365725f6964a0",
            "UserIdResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_user_profile_response() {
        let resp = UserProfileResponse {
            id: String::new(),
            email: String::new(),
            display_name: String::new(),
            avatar_url: None,
            role: String::new(),
            orgs: vec![],
        };
        let encoded = codec::encode(&resp).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "86a26964a0a5656d61696ca0ac646973706c61795f6e616d65a0aa6176617461725f75726cc0a4726f6c65a0a46f72677390",
            "UserProfileResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_org_summary() {
        let org = OrgSummary {
            name: String::new(),
            verified_via: None,
            verified_ref: None,
            is_reserved: false,
        };
        let encoded = codec::encode(&org).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "84a46e616d65a0ac76657269666965645f766961c0ac76657269666965645f726566c0ab69735f7265736572766564c2",
            "OrgSummary schema changed — review consumer impact before updating this literal"
        );
    }
}
