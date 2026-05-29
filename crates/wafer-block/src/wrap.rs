//! WRAP — WAFER Resource Access Policy.
//!
//! Enforces resource-level access control in the runtime's `call_block()` dispatch.
//! Client wrappers set `wrap.resource` meta on the message; the runtime reads it
//! and calls `check_access()` before dispatching to the handler.

use crate::{types::ResourceGrant, ErrorCode, WaferError};

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

/// Extract the owning block id from a storage path of the form
/// `{org}/{block}/{rest}` (with optional leading `@` used by SolobaseStorageBlock
/// to mark cross-block access in source code).
///
/// Returns `None` if the path doesn't have at least two slash-separated
/// segments.
///
/// ```ignore
/// assert_eq!(storage_resource_owner("suppers-ai/files/photos/a.png"),
///            Some("suppers-ai/files".to_string()));
/// assert_eq!(storage_resource_owner("@wafer-run/web/public/index.html"),
///            Some("wafer-run/web".to_string()));
/// assert_eq!(storage_resource_owner("just-one-segment"), None);
/// ```
pub fn storage_resource_owner(path: &str) -> Option<String> {
    let path = path.strip_prefix('@').unwrap_or(path);
    let mut parts = path.splitn(3, '/');
    let org = parts.next()?;
    let block = parts.next()?;
    if org.is_empty() || block.is_empty() {
        return None;
    }
    Some(format!("{org}/{block}"))
}

/// Dispatch to the right resource-owner parser for the given resource type.
///
/// `ResourceType::Storage` parses slash-separated `{org}/{block}/...` paths
/// via [`storage_resource_owner`]. Everything else (Db, Config, untyped)
/// parses double-underscore `{org}__{block}__...` names via [`resource_owner`].
///
/// Used by `check_access` and by the lifecycle grant validator to apply
/// ownership rules consistently across resource types.
pub fn typed_resource_owner(
    resource: &str,
    resource_type: Option<&crate::types::ResourceType>,
) -> Option<String> {
    match resource_type {
        Some(crate::types::ResourceType::Storage) => storage_resource_owner(resource),
        _ => resource_owner(resource),
    }
}

/// Check whether `caller_id` is allowed to access `resource`.
///
/// For namespace-based resources (Db, Config, or untyped):
/// 1. `__raw_sql__` → admin-only (exact match on `admin_block`)
/// 2. `__ddl__` → any attributable caller (NOT admin-only). Convention is that
///    blocks only DDL their own (`{org}__{block}__*`) tables; this is enforced
///    by code review + the WRAP-grant audit script, not by parsing SQL here.
/// 3. `SOLOBASE_SHARED__*` → any block reads, admin-only writes
/// 4. Own resource (`resource_owner(resource) == caller_id`) → Ok
/// 5. Admin (`caller_id == admin_block`) → Ok
/// 6. Grant match (grantee + resource pattern + write flag) → Ok
/// 7. Unnamespaced (`resource_owner()` returns `None`) → Err
/// 8. Otherwise → Err
///
/// For Storage (slash-separated `{org}/{block}/...` paths):
/// 1. Own-namespace self-admit (`storage_resource_owner(resource) == caller_id`) → Ok
/// 2. Admin → Ok
/// 3. Grant match → Ok
/// 4. Otherwise → Err (default deny)
///
/// For Network and Crypto (URLs, operation names — not namespaced):
/// 1. Admin → Ok
/// 2. Grant match → Ok
/// 3. Otherwise → Err (default deny)
pub fn check_access(
    caller_id: Option<&str>,
    resource: &str,
    is_write: bool,
    resource_type: Option<&crate::types::ResourceType>,
    grants: &[ResourceGrant],
    admin_block: &str,
) -> Result<(), WaferError> {
    // Namespace-based rules only apply to Db, Config, or untyped resources.
    // Network, Storage, and Crypto resources use URLs / file-paths /
    // operation-names, not the {org}__{block}__{name} convention.
    let namespace_based = !matches!(
        resource_type,
        Some(crate::types::ResourceType::Network)
            | Some(crate::types::ResourceType::Storage)
            | Some(crate::types::ResourceType::Crypto)
    );

    if namespace_based {
        // Rule 1: raw SQL is admin-only
        if resource == "__raw_sql__" {
            return match caller_id {
                Some(c) if c == admin_block => Ok(()),
                _ => Err(WaferError::new(
                    ErrorCode::PERMISSION_DENIED,
                    format!(
                        "WRAP: raw SQL access denied (caller: {caller_id:?}, admin: {admin_block})"
                    ),
                )),
            };
        }

        // Rule 1a: DDL is open to any attributable caller. Each block is expected
        // to DDL only its own tables on init (`{org}__{block}__*`); cross-block
        // DDL is a misuse caught by code review + the WRAP-grant audit script,
        // not by parsing SQL here. Anonymous callers (no `caller_id`) are still
        // denied — DDL needs an attributable owner.
        if resource == "__ddl__" {
            return match caller_id {
                Some(_) => Ok(()),
                None => Err(WaferError::new(
                    ErrorCode::PERMISSION_DENIED,
                    "WRAP: DDL requires an attributable caller (caller: None)".to_string(),
                )),
            };
        }

        // Rule 2: SOLOBASE_SHARED__ resources
        //
        // Writes: admin only.
        // Reads: any *attributable* caller (caller_id.is_some()). Anonymous
        // callers (None) are denied — shared config may carry secrets and
        // there is no reason an unauthenticated context should read them.
        let lower = resource.to_lowercase();
        if lower.starts_with("solobase_shared__") {
            if is_write {
                return match caller_id {
                    Some(c) if c == admin_block => Ok(()),
                    _ => Err(WaferError::new(
                        ErrorCode::PERMISSION_DENIED,
                        format!(
                            "WRAP: only admin can write SOLOBASE_SHARED__ resources (caller: {caller_id:?})"
                        ),
                    )),
                };
            }
            return match caller_id {
                Some(_) => Ok(()),
                None => Err(WaferError::new(
                    ErrorCode::PERMISSION_DENIED,
                    format!(
                        "WRAP: SOLOBASE_SHARED__ read denied for anonymous caller (resource: {resource})"
                    ),
                )),
            };
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
                format!("WRAP: unnamespaced resource '{resource}' denied (caller: {caller_id:?})"),
            ));
        }

        // Rule 7: no match → deny
        return Err(WaferError::new(
            ErrorCode::PERMISSION_DENIED,
            format!("WRAP: access denied on '{resource}' (caller: {caller_id:?})"),
        ));
    }

    // --- Non-namespace resources (Network, Storage, Crypto) ---
    // Storage gets Rule 3 self-admit on `{org}/{block}/...` paths;
    // Network + Crypto fall straight through to admin + grant matching.

    // Rule 3 (Storage only): own-namespace self-admit.
    // Caller `{org}/{block}` accessing `{org}/{block}/...` is auto-allowed.
    // Cross-block storage access still falls through to grant matching.
    if matches!(resource_type, Some(crate::types::ResourceType::Storage)) {
        if let Some(owner) = storage_resource_owner(resource) {
            if caller_id == Some(owner.as_str()) {
                return Ok(());
            }
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

    // Default deny
    Err(WaferError::new(
        ErrorCode::PERMISSION_DENIED,
        format!(
            "WRAP: access denied on '{resource}' (caller: {caller_id:?}, type: {resource_type:?})"
        ),
    ))
}

fn grant_matches_grantee(grantee: &str, caller: &str) -> bool {
    grantee == "*" || grantee == caller
}

/// Pattern matching for grant resource patterns.
///
/// Two modes:
/// 1. URL patterns (pattern starts with `http://` or `https://`) — parsed
///    with the `url` crate and matched structurally: scheme + host + path.
///    Subdomain wildcards (`*.example.com`) match exactly one label.
/// 2. Non-URL patterns (storage paths, namespaces) — substring glob with
///    `*` wildcards, used for things like `wafer-run/web/*` or
///    `suppers_ai__auth__*`.
///
/// The URL branch closes the substring-glob bypass (SEC-007): patterns
/// like `https://*.example.com/*` no longer match attacker-controlled
/// strings like `https://example.com.attacker.com/...`.
///
/// Supported URL patterns:
/// - `https://api.stripe.com/*` — any path under exact host
/// - `https://*.example.com/*` — single-level subdomain wildcard
/// - `https://**.example.com/*` — multi-level subdomain wildcard
/// - `https://example.com/v1/items` — exact match (no wildcard)
///
/// Returns `false` for malformed URL patterns (cannot match anything).
fn grant_matches_resource(pattern: &str, resource: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    // URL-shaped patterns get structured matching.
    if is_url_pattern(pattern) {
        return url_pattern_matches(pattern, resource);
    }

    // Non-URL patterns: substring glob (storage paths, namespace prefixes, etc).
    glob_matches(pattern, resource)
}

fn is_url_pattern(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Substring-glob matcher for non-URL patterns (storage paths, namespaces).
///
/// `*` is a wildcard. First segment must anchor to the start. If the
/// pattern doesn't end in `*`, the resource must also end at the final
/// segment.
fn glob_matches(pattern: &str, resource: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return resource == pattern;
    }

    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if let Some(found) = resource[pos..].find(part) {
            if i == 0 && found != 0 {
                return false;
            }
            pos += found + part.len();
        } else {
            return false;
        }
    }

    if !pattern.ends_with('*') {
        return pos == resource.len();
    }
    true
}

/// Structured URL pattern matcher.
///
/// - Scheme: exact match (case-insensitive — `url` crate normalizes).
/// - Host:
///   - `**.example.com` — matches `example.com` and any depth of subdomains
///   - `*.example.com`  — matches exactly one extra label (`a.example.com`,
///     not `a.b.example.com`, not `example.com`)
///   - `example.com`    — exact match
///   - `*`              — matches any host
/// - Path: prefix match if pattern ends with `/*`; otherwise exact.
///
/// The `url` crate parses both sides, which means `example.com.evil.com`
/// becomes a literal host (it's not interpreted as a subdomain of
/// `example.com`) — that's exactly what closes the SEC-007 bypass.
fn url_pattern_matches(pattern: &str, resource: &str) -> bool {
    // Replace `*` with a placeholder that's valid in both host labels and
    // URL paths so the pattern parses cleanly. `url::Url` lowercases hosts,
    // so the sentinel must be lowercase as well to survive the round-trip.
    const STAR_SENTINEL: &str = "wafer-star-placeholder";

    let prepared_pattern = pattern.replace('*', STAR_SENTINEL);
    let Ok(parsed_pattern) = url::Url::parse(&prepared_pattern) else {
        return false;
    };
    let Ok(parsed_resource) = url::Url::parse(resource) else {
        return false;
    };

    // Scheme: exact.
    if parsed_pattern.scheme() != parsed_resource.scheme() {
        return false;
    }

    // Host: extract the original (un-sentinel-ed) host pattern.
    let pattern_host = parsed_pattern
        .host_str()
        .map(|h| h.replace(STAR_SENTINEL, "*"));
    let resource_host = parsed_resource.host_str();
    if !host_matches(pattern_host.as_deref(), resource_host) {
        return false;
    }

    // Path: extract the original path with `*` restored.
    let pattern_path = parsed_pattern.path().replace(STAR_SENTINEL, "*");
    let resource_path = parsed_resource.path();
    path_matches(&pattern_path, resource_path)
}

fn host_matches(pattern: Option<&str>, resource: Option<&str>) -> bool {
    match (pattern, resource) {
        (Some(p), Some(r)) => host_pattern_matches(p, r),
        (None, None) => true,
        // One has a host, the other doesn't — no match.
        _ => false,
    }
}

fn host_pattern_matches(pattern: &str, host: &str) -> bool {
    // url::Url lowercases hosts; lowercase pattern for symmetry.
    let pattern = pattern.to_ascii_lowercase();
    let host = host.to_ascii_lowercase();

    if pattern == "*" {
        return true;
    }

    // Multi-level: `**.example.com` matches `example.com` and any subdomain
    // depth.
    if let Some(suffix) = pattern.strip_prefix("**.") {
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }

    // Single-level: `*.example.com` matches exactly one extra label.
    if let Some(suffix) = pattern.strip_prefix("*.") {
        let Some(rest) = host.strip_suffix(&format!(".{suffix}")) else {
            return false;
        };
        // `rest` is the matched wildcard label — must be non-empty and
        // contain no `.` (single level only).
        return !rest.is_empty() && !rest.contains('.');
    }

    pattern == host
}

fn path_matches(pattern: &str, path: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/*") {
        // `/*` matches the prefix followed by `/` and anything else,
        // OR the prefix exactly (so `/api/*` matches `/api` too if you
        // consider the trailing slash implied). Be strict: require the
        // `/` separator.
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if pattern == "*" {
        return true;
    }
    pattern == path
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
    fn test_ddl_permissive_for_any_block() {
        let grants = vec![];
        let admin = "suppers-ai/admin";
        // Non-admin block can DDL its own tables (write).
        assert!(check_access(
            Some("suppers-ai/auth"),
            "__ddl__",
            true,
            None,
            &grants,
            admin
        )
        .is_ok());
        // Another non-admin block likewise.
        assert!(check_access(
            Some("suppers-ai/files"),
            "__ddl__",
            true,
            None,
            &grants,
            admin
        )
        .is_ok());
        // Admin too (sanity).
        assert!(check_access(Some(admin), "__ddl__", true, None, &grants, admin).is_ok());
        // Anonymous (no caller) is still denied — DDL needs an attributable caller.
        assert!(check_access(None, "__ddl__", true, None, &grants, admin).is_err());
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
        // Any *attributable* block can read shared
        assert!(check_access(
            Some("suppers-ai/auth"),
            "SOLOBASE_SHARED__APP_NAME",
            false,
            None,
            &grants,
            admin
        )
        .is_ok());
        // Anonymous (no caller_id) cannot read shared — shared config may
        // contain secrets and there's no reason an unauthenticated context
        // should access it.
        assert!(check_access(
            None,
            "SOLOBASE_SHARED__APP_NAME",
            false,
            None,
            &grants,
            admin
        )
        .is_err());
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

        // SEC-007 regression: substring-glob bypasses must be blocked.
        // Attacker-controlled hostnames that *contain* the pattern host
        // (suffix or prefix) must NOT match.
        assert!(
            !grant_matches_resource(
                "https://*.example.com/*",
                "https://example.com.evil.com/bar"
            ),
            "example.com.evil.com must not match *.example.com"
        );
        assert!(
            !grant_matches_resource(
                "https://*.example.com/*",
                "https://evil.example.com.attacker.com/bar"
            ),
            "evil.example.com.attacker.com must not match *.example.com (cited SEC-007 attack)"
        );

        // Single-level subdomain wildcard must NOT match multi-level
        // subdomains. (Multi-level uses `**`.)
        assert!(
            !grant_matches_resource("https://*.example.com/*", "https://foo.bar.example.com/x"),
            "*.example.com must NOT match deeply-nested subdomains"
        );

        // Multi-level subdomain wildcard via `**.example.com`.
        assert!(grant_matches_resource(
            "https://**.example.com/*",
            "https://foo.bar.example.com/x"
        ));
        assert!(grant_matches_resource(
            "https://**.example.com/*",
            "https://example.com/x"
        ));
        assert!(!grant_matches_resource(
            "https://**.example.com/*",
            "https://example.com.evil.com/x"
        ));

        // Plain prefix patterns still work (no wildcard in host).
        assert!(grant_matches_resource(
            "https://api.example.com/*",
            "https://api.example.com/v1/items"
        ));
        assert!(!grant_matches_resource(
            "https://api.example.com/*",
            "https://other.example.com/v1/items"
        ));

        // Exact URL match (no trailing wildcard).
        assert!(grant_matches_resource(
            "https://api.example.com/v1",
            "https://api.example.com/v1"
        ));
        assert!(!grant_matches_resource(
            "https://api.example.com/v1",
            "https://api.example.com/v2"
        ));

        // Invalid URL pattern → no match.
        assert!(!grant_matches_resource(
            "https://[bad-pattern",
            "https://anything.example.com/"
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
    fn test_crypto_resource_type() {
        let admin = "suppers-ai/admin";
        let crypto = Some(&ResourceType::Crypto);

        // No grants → denied (default deny for crypto, like network/storage)
        assert!(check_access(Some("suppers-ai/auth"), "sign", false, crypto, &[], admin).is_err());

        // Wildcard grant for all blocks on all crypto ops → allowed
        let grants = vec![ResourceGrant::read("*", "*").typed(ResourceType::Crypto)];
        assert!(check_access(
            Some("suppers-ai/auth"),
            "sign",
            false,
            crypto,
            &grants,
            admin
        )
        .is_ok());
        assert!(check_access(
            Some("suppers-ai/auth"),
            "random_bytes",
            false,
            crypto,
            &grants,
            admin
        )
        .is_ok());

        // Operation-specific grant: only random_bytes, not sign
        let grants = vec![
            ResourceGrant::read("suppers-ai/auth", "random_bytes").typed(ResourceType::Crypto)
        ];
        assert!(check_access(
            Some("suppers-ai/auth"),
            "random_bytes",
            false,
            crypto,
            &grants,
            admin
        )
        .is_ok());
        assert!(check_access(
            Some("suppers-ai/auth"),
            "sign",
            false,
            crypto,
            &grants,
            admin
        )
        .is_err());

        // Admin always allowed
        assert!(check_access(Some(admin), "sign", false, crypto, &[], admin).is_ok());

        // Crypto grant doesn't satisfy a Db request — the typed match fails
        // and there's no namespace fallback for an unnamespaced resource like
        // "sign" / "random_bytes".
        let grants = vec![ResourceGrant::read("*", "*").typed(ResourceType::Crypto)];
        assert!(check_access(
            Some("suppers-ai/auth"),
            "sign",
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

        // Own-namespace storage access via Rule 3 (Wave 26 / c18):
        // caller `{org}/{block}` reaching `{org}/{block}/...` is auto-allowed
        // without any explicit grant.
        assert!(check_access(
            Some("wafer-run/web"),
            "wafer-run/web/public",
            false,
            storage,
            &[],
            admin
        )
        .is_ok());
        // Including writes
        assert!(check_access(
            Some("wafer-run/web"),
            "wafer-run/web/public/index.html",
            true,
            storage,
            &[],
            admin
        )
        .is_ok());
        // Cross-block storage still requires a grant
        assert!(check_access(
            Some("wafer-run/web"),
            "suppers-ai/files/photos/a.png",
            false,
            storage,
            &[],
            admin
        )
        .is_err());
    }

    #[test]
    fn test_storage_resource_owner_parser() {
        assert_eq!(
            storage_resource_owner("suppers-ai/files/photos/a.png"),
            Some("suppers-ai/files".to_string())
        );
        assert_eq!(
            storage_resource_owner("@wafer-run/web/public/index.html"),
            Some("wafer-run/web".to_string())
        );
        // Exactly two segments — owner is the whole thing
        assert_eq!(
            storage_resource_owner("foo/bar"),
            Some("foo/bar".to_string())
        );
        // Fewer than two segments returns None
        assert_eq!(storage_resource_owner("just-one-segment"), None);
        assert_eq!(storage_resource_owner(""), None);
        assert_eq!(storage_resource_owner("/leading-slash"), None);
    }

    #[test]
    fn test_typed_resource_owner_dispatch() {
        // Storage → slash parser
        assert_eq!(
            typed_resource_owner(
                "suppers-ai/files/photos/a.png",
                Some(&ResourceType::Storage)
            ),
            Some("suppers-ai/files".to_string())
        );
        // Db / untyped → __-parser
        assert_eq!(
            typed_resource_owner("suppers_ai__auth__users", Some(&ResourceType::Db)),
            Some("suppers-ai/auth".to_string())
        );
        assert_eq!(
            typed_resource_owner("suppers_ai__auth__users", None),
            Some("suppers-ai/auth".to_string())
        );
        // Mismatched format returns None
        assert_eq!(
            typed_resource_owner("suppers-ai/files/photos", Some(&ResourceType::Db)),
            None
        );
        assert_eq!(
            typed_resource_owner("suppers_ai__auth__users", Some(&ResourceType::Storage)),
            None
        );
    }
}
