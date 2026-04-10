//! WRAP — WAFER Resource Access Policy.
//!
//! Enforces resource-level access control in the runtime's `call_block()` dispatch.
//! Client wrappers set `wrap.resource` meta on the message; the runtime reads it
//! and calls `check_access()` before dispatching to the handler.

use crate::types::ResourceGrant;
use crate::{ErrorCode, WaferError};

/// Extract the owning block ID from a namespaced resource name.
///
/// Convention: `suppers_ai__auth__users` → `suppers-ai/auth`
///
/// Splits on the first two `__` segments, lowercases, converts `__` → `/`
/// and `_` → `-`. Returns `None` if the name doesn't have at least two `__`
/// separators (i.e. `{org}__{block}__{resource}`).
pub fn resource_owner(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let first = lower.find("__")?;
    let rest = &lower[first + 2..];
    let second = rest.find("__")?;
    // prefix = "suppers_ai__auth"
    let prefix = &lower[..first + 2 + second];
    Some(prefix.replace("__", "/").replace('_', "-"))
}

/// Convert a block ID to its resource name prefix.
///
/// `suppers-ai/auth` → `suppers_ai__auth__`
pub fn resource_prefix(block_id: &str) -> String {
    let mut prefix = block_id.replace('/', "__").replace('-', "_");
    prefix.push_str("__");
    prefix
}

/// Check whether `caller_id` is allowed to access `resource`.
///
/// For namespace-based resources (Db, Config, Crypto, or untyped):
/// 1. `__raw_sql__` → admin-only (exact match on `admin_block`)
/// 2. `SOLOBASE_SHARED__*` → any block reads, admin-only writes
/// 3. Own resource (`resource_owner(resource) == caller_id`) → Ok
/// 4. Admin (`caller_id == admin_block`) → Ok
/// 5. Grant match (grantee + resource pattern + write flag) → Ok
/// 6. Unnamespaced (`resource_owner()` returns `None`) → Err
/// 7. Otherwise → Err
///
/// For non-namespace resources (Network, Storage):
/// 4. Admin → Ok
/// 5. Grant match → Ok
/// 7. Otherwise → Err (default deny)
pub fn check_access(
    caller_id: Option<&str>,
    resource: &str,
    is_write: bool,
    resource_type: Option<&crate::types::ResourceType>,
    grants: &[ResourceGrant],
    admin_block: &str,
) -> Result<(), WaferError> {
    // Namespace-based rules only apply to Db, Config, Crypto, or untyped resources.
    // Network and Storage resources use URLs/paths, not the {org}__{block}__{name} convention.
    let namespace_based = !matches!(
        resource_type,
        Some(crate::types::ResourceType::Network) | Some(crate::types::ResourceType::Storage)
    );

    if namespace_based {
        // Rule 1: raw SQL is admin-only
        if resource == "__raw_sql__" {
            return match caller_id {
                Some(c) if c == admin_block => Ok(()),
                _ => Err(WaferError::new(
                    ErrorCode::PERMISSION_DENIED,
                    format!(
                        "WRAP: raw SQL access denied (caller: {:?}, admin: {})",
                        caller_id, admin_block
                    ),
                )),
            };
        }

        // Rule 2: SOLOBASE_SHARED__ resources
        let lower = resource.to_lowercase();
        if lower.starts_with("solobase_shared__") {
            if is_write {
                return match caller_id {
                    Some(c) if c == admin_block => Ok(()),
                    _ => Err(WaferError::new(
                        ErrorCode::PERMISSION_DENIED,
                        format!(
                            "WRAP: only admin can write SOLOBASE_SHARED__ resources (caller: {:?})",
                            caller_id
                        ),
                    )),
                };
            }
            return Ok(());
        }

        // Rule 3: own resource
        let owner = resource_owner(resource);
        if let Some(ref owner) = owner {
            if caller_id == Some(owner.as_str()) {
                return Ok(());
            }
        }

        // Rule 4: admin block has full access
        if caller_id == Some(admin_block) {
            return Ok(());
        }

        // Rule 5: grant match
        if let Some(caller) = caller_id {
            for grant in grants {
                if !grant_matches_grantee(&grant.grantee, caller) {
                    continue;
                }
                if !grant_matches_resource(&grant.resource, resource) {
                    continue;
                }
                if is_write && !grant.write {
                    continue;
                }
                if let Some(ref grant_type) = grant.resource_type {
                    match resource_type {
                        Some(req_type) if grant_type == req_type => {}
                        _ => continue,
                    }
                }
                return Ok(());
            }
        }

        // Rule 6: unnamespaced resource → deny
        if owner.is_none() {
            return Err(WaferError::new(
                ErrorCode::PERMISSION_DENIED,
                format!(
                    "WRAP: unnamespaced resource '{}' denied (caller: {:?})",
                    resource, caller_id
                ),
            ));
        }

        // Rule 7: no match → deny
        return Err(WaferError::new(
            ErrorCode::PERMISSION_DENIED,
            format!(
                "WRAP: access denied on '{}' (caller: {:?})",
                resource, caller_id
            ),
        ));
    }

    // --- Non-namespace resources (Network, Storage) ---
    // Only admin check + grant matching, default deny.

    // Rule 4: admin block has full access
    if caller_id == Some(admin_block) {
        return Ok(());
    }

    // Rule 5: grant match
    if let Some(caller) = caller_id {
        for grant in grants {
            if !grant_matches_grantee(&grant.grantee, caller) {
                continue;
            }
            if !grant_matches_resource(&grant.resource, resource) {
                continue;
            }
            if is_write && !grant.write {
                continue;
            }
            if let Some(ref grant_type) = grant.resource_type {
                match resource_type {
                    Some(req_type) if grant_type == req_type => {}
                    _ => continue,
                }
            }
            return Ok(());
        }
    }

    // Default deny
    Err(WaferError::new(
        ErrorCode::PERMISSION_DENIED,
        format!(
            "WRAP: access denied on '{}' (caller: {:?}, type: {:?})",
            resource, caller_id, resource_type
        ),
    ))
}

fn grant_matches_grantee(grantee: &str, caller: &str) -> bool {
    grantee == "*" || grantee == caller
}

/// Glob-style pattern matching for grant resource patterns.
///
/// Supports multiple `*` wildcards:
/// - `*` alone matches everything
/// - `https://api.stripe.com/*` matches any path under that domain
/// - `https://*.example.com/*` matches any subdomain + path
/// - `wafer-run/web/public/*` matches any key under that storage path
/// - No wildcard = exact match
fn grant_matches_resource(pattern: &str, resource: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        // No wildcard — exact match
        return resource == pattern;
    }

    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if let Some(found) = resource[pos..].find(part) {
            if i == 0 && found != 0 {
                // First segment must match at start
                return false;
            }
            pos += found + part.len();
        } else {
            return false;
        }
    }

    // If pattern doesn't end with *, resource must end at pos
    if !pattern.ends_with('*') {
        return pos == resource.len();
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ResourceType;

    #[test]
    fn test_resource_owner() {
        assert_eq!(
            resource_owner("suppers_ai__auth__users"),
            Some("suppers-ai/auth".to_string())
        );
        assert_eq!(
            resource_owner("SUPPERS_AI__AUTH__JWT_SECRET"),
            Some("suppers-ai/auth".to_string())
        );
        assert_eq!(
            resource_owner("suppers_ai__admin__roles"),
            Some("suppers-ai/admin".to_string())
        );
        // Not enough __ segments
        assert_eq!(resource_owner("auth_users"), None);
        assert_eq!(resource_owner("simple"), None);
        // SOLOBASE_SHARED has only one __ before the var name
        assert_eq!(resource_owner("SOLOBASE_SHARED__APP_NAME"), None);
    }

    #[test]
    fn test_resource_prefix() {
        assert_eq!(resource_prefix("suppers-ai/auth"), "suppers_ai__auth__");
        assert_eq!(resource_prefix("wafer-run/web"), "wafer_run__web__");
    }

    #[test]
    fn test_raw_sql_admin_only() {
        let grants = vec![];
        // Admin can use raw SQL
        assert!(check_access(
            Some("suppers-ai/admin"),
            "__raw_sql__",
            false,
            None,
            &grants,
            "suppers-ai/admin"
        )
        .is_ok());
        // Non-admin cannot
        assert!(check_access(
            Some("suppers-ai/auth"),
            "__raw_sql__",
            false,
            None,
            &grants,
            "suppers-ai/admin"
        )
        .is_err());
        // No caller cannot
        assert!(check_access(
            None,
            "__raw_sql__",
            false,
            None,
            &grants,
            "suppers-ai/admin"
        )
        .is_err());
    }

    #[test]
    fn test_shared_resources() {
        let grants = vec![];
        let admin = "suppers-ai/admin";
        // Any block can read shared
        assert!(check_access(
            Some("suppers-ai/auth"),
            "SOLOBASE_SHARED__APP_NAME",
            false,
            None,
            &grants,
            admin
        )
        .is_ok());
        // Only admin can write shared
        assert!(check_access(
            Some("suppers-ai/auth"),
            "SOLOBASE_SHARED__APP_NAME",
            true,
            None,
            &grants,
            admin
        )
        .is_err());
        assert!(check_access(
            Some("suppers-ai/admin"),
            "SOLOBASE_SHARED__APP_NAME",
            true,
            None,
            &grants,
            admin
        )
        .is_ok());
    }

    #[test]
    fn test_own_resource() {
        let grants = vec![];
        let admin = "suppers-ai/admin";
        // Auth block can access its own resources
        assert!(check_access(
            Some("suppers-ai/auth"),
            "suppers_ai__auth__users",
            true,
            None,
            &grants,
            admin
        )
        .is_ok());
        // But not another block's resources
        assert!(check_access(
            Some("suppers-ai/auth"),
            "suppers_ai__admin__roles",
            false,
            None,
            &grants,
            admin
        )
        .is_err());
    }

    #[test]
    fn test_admin_full_access() {
        let grants = vec![];
        let admin = "suppers-ai/admin";
        assert!(check_access(
            Some(admin),
            "suppers_ai__auth__users",
            true,
            None,
            &grants,
            admin
        )
        .is_ok());
    }

    #[test]
    fn test_grant_matching() {
        let admin = "suppers-ai/admin";
        // Read grant: admin can read auth users
        let grants = vec![ResourceGrant::read(
            "suppers-ai/admin",
            "suppers_ai__auth__users",
        )];
        assert!(check_access(
            Some("suppers-ai/admin"),
            "suppers_ai__auth__users",
            false,
            None,
            &grants,
            "some-other/admin" // not admin for this test
        )
        .is_ok());
        // Write denied with read-only grant
        assert!(check_access(
            Some("suppers-ai/admin"),
            "suppers_ai__auth__users",
            true,
            None,
            &grants,
            "some-other/admin"
        )
        .is_err());

        // Wildcard grant
        let grants = vec![ResourceGrant::read(
            "suppers-ai/admin",
            "suppers_ai__auth__*",
        )];
        assert!(check_access(
            Some("suppers-ai/admin"),
            "suppers_ai__auth__users",
            false,
            None,
            &grants,
            "some-other/admin"
        )
        .is_ok());
        assert!(check_access(
            Some("suppers-ai/admin"),
            "suppers_ai__auth__tokens",
            false,
            None,
            &grants,
            "some-other/admin"
        )
        .is_ok());

        // Wildcard grantee
        let grants = vec![ResourceGrant::read("*", "suppers_ai__admin__network_rules")];
        assert!(check_access(
            Some("suppers-ai/files"),
            "suppers_ai__admin__network_rules",
            false,
            None,
            &grants,
            admin
        )
        .is_ok());
    }

    #[test]
    fn test_unnamespaced_denied() {
        let grants = vec![];
        let admin = "suppers-ai/admin";
        // Unnamespaced resource names are denied in strict mode
        assert!(check_access(
            Some("suppers-ai/auth"),
            "auth_users",
            false,
            None,
            &grants,
            admin
        )
        .is_err());
    }

    #[test]
    fn test_grant_matches_resource_glob() {
        // Wildcard all
        assert!(grant_matches_resource("*", "https://example.com"));
        assert!(grant_matches_resource("*", "anything"));

        // Exact match
        assert!(grant_matches_resource(
            "https://api.stripe.com/v1",
            "https://api.stripe.com/v1"
        ));
        assert!(!grant_matches_resource(
            "https://api.stripe.com/v1",
            "https://api.stripe.com/v2"
        ));

        // Trailing wildcard (URL)
        assert!(grant_matches_resource(
            "https://api.stripe.com/*",
            "https://api.stripe.com/v1/charges"
        ));
        assert!(!grant_matches_resource(
            "https://api.stripe.com/*",
            "https://evil.com/api.stripe.com/"
        ));

        // Middle wildcard (subdomain)
        assert!(grant_matches_resource(
            "https://*.example.com/*",
            "https://api.example.com/v1"
        ));
        assert!(!grant_matches_resource(
            "https://*.example.com/*",
            "http://api.example.com/v1"
        ));

        // Storage path patterns
        assert!(grant_matches_resource(
            "wafer-run/web/*",
            "wafer-run/web/public"
        ));
        assert!(grant_matches_resource(
            "wafer-run/web/*",
            "wafer-run/web/public/index.html"
        ));
        assert!(!grant_matches_resource(
            "wafer-run/web/*",
            "suppers-ai/files/uploads"
        ));

        // No match
        assert!(!grant_matches_resource(
            "https://safe.com/*",
            "https://evil.com/safe.com/"
        ));

        // Namespace patterns (backward compat)
        assert!(grant_matches_resource(
            "suppers_ai__auth__*",
            "suppers_ai__auth__users"
        ));
        assert!(grant_matches_resource(
            "suppers_ai__auth__*",
            "suppers_ai__auth__tokens"
        ));
    }

    #[test]
    fn test_read_write_grant() {
        let grants = vec![ResourceGrant::read_write(
            "suppers-ai/auth",
            "suppers_ai__admin__user_roles",
        )];
        let admin = "some-other/admin";
        // Read OK
        assert!(check_access(
            Some("suppers-ai/auth"),
            "suppers_ai__admin__user_roles",
            false,
            None,
            &grants,
            admin
        )
        .is_ok());
        // Write OK
        assert!(check_access(
            Some("suppers-ai/auth"),
            "suppers_ai__admin__user_roles",
            true,
            None,
            &grants,
            admin
        )
        .is_ok());
    }

    #[test]
    fn test_network_resource_type() {
        let admin = "suppers-ai/admin";
        let net = Some(&ResourceType::Network);

        // No grants → denied (default deny for network)
        assert!(check_access(
            Some("suppers-ai/products"),
            "https://api.stripe.com/v1/charges",
            false,
            net,
            &[],
            admin
        )
        .is_err());

        // Wildcard grant for all blocks → allowed
        let grants = vec![ResourceGrant::read("*", "*").typed(ResourceType::Network)];
        assert!(check_access(
            Some("suppers-ai/products"),
            "https://api.stripe.com/v1/charges",
            false,
            net,
            &grants,
            admin
        )
        .is_ok());

        // Specific URL grant
        let grants = vec![
            ResourceGrant::read("suppers-ai/products", "https://api.stripe.com/*")
                .typed(ResourceType::Network),
        ];
        assert!(check_access(
            Some("suppers-ai/products"),
            "https://api.stripe.com/v1/charges",
            false,
            net,
            &grants,
            admin
        )
        .is_ok());
        // Different block → denied
        assert!(check_access(
            Some("suppers-ai/auth"),
            "https://api.stripe.com/v1/charges",
            false,
            net,
            &grants,
            admin
        )
        .is_err());
        // Different URL → denied
        assert!(check_access(
            Some("suppers-ai/products"),
            "https://evil.com/steal",
            false,
            net,
            &grants,
            admin
        )
        .is_err());

        // Admin always allowed
        assert!(check_access(Some(admin), "https://anything.com", false, net, &[], admin).is_ok());

        // Network grant doesn't satisfy Db request
        let grants = vec![ResourceGrant::read("*", "*").typed(ResourceType::Network)];
        assert!(check_access(
            Some("suppers-ai/auth"),
            "auth_users",
            false,
            None, // untyped / Db
            &grants,
            admin
        )
        .is_err());
    }

    #[test]
    fn test_storage_resource_type() {
        let admin = "suppers-ai/admin";
        let storage = Some(&ResourceType::Storage);

        // No grants → denied
        assert!(check_access(
            Some("suppers-ai/files"),
            "wafer-run/web/public",
            false,
            storage,
            &[],
            admin
        )
        .is_err());

        // Grant for specific path
        let grants =
            vec![ResourceGrant::read("suppers-ai/files", "wafer-run/web/*")
                .typed(ResourceType::Storage)];
        assert!(check_access(
            Some("suppers-ai/files"),
            "wafer-run/web/public",
            false,
            storage,
            &grants,
            admin
        )
        .is_ok());
        // Write denied (read-only grant)
        assert!(check_access(
            Some("suppers-ai/files"),
            "wafer-run/web/public",
            true,
            storage,
            &grants,
            admin
        )
        .is_err());

        // URL-like resource should NOT be treated as namespaced
        // (no "own resource" shortcut for storage paths)
        assert!(check_access(
            Some("wafer-run/web"),
            "wafer-run/web/public",
            false,
            storage,
            &[], // no grants
            admin
        )
        .is_err());
    }
}
