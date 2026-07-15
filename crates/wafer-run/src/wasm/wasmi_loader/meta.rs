//! Guest meta (HTTP header) sanitisation shared by the host-import linker
//! and the [`super::WasmiBlock`] dispatch. WASM blocks may only read/write
//! header-derived meta keys they have been granted via `HeaderPolicy`.

use wafer_block::core_types::*;

use crate::wasm::capabilities::BlockCapabilities;

// ---------------------------------------------------------------------------
// Guest meta sanitisation
// ---------------------------------------------------------------------------

/// Default set of HTTP header names that are considered security-sensitive.
/// WASM blocks cannot read or write these unless they declare them in their
/// `HeaderPolicy.readable` / `HeaderPolicy.writable` (and are granted the
/// cap after config intersection).
pub(crate) fn default_sensitive_headers() -> &'static [&'static str] {
    &[
        "authorization",
        "cookie",
        "set-cookie",
        "location",
        "access-control-allow-origin",
        "access-control-allow-credentials",
        "access-control-allow-methods",
        "access-control-allow-headers",
        "access-control-expose-headers",
        "access-control-max-age",
        "strict-transport-security",
        "x-frame-options",
        "content-security-policy",
        "content-security-policy-report-only",
    ]
}

fn is_sensitive_header(name: &str, policy_masked: &[String]) -> bool {
    let n = name.to_lowercase();
    default_sensitive_headers().contains(&n.as_str())
        || policy_masked.iter().any(|m| m.eq_ignore_ascii_case(&n))
}

/// Extract the canonical (lowercase) HTTP header name from a wafer meta key,
/// or `None` if the key is not a header.
///
/// Matches three forms:
/// - `req.header.{name}` — inbound request header
/// - `resp.header.{name}` — outbound response header
/// - `resp.set_cookie` / `resp.set_cookie.*` — legacy cookie keys, mapped to `set-cookie`
pub(crate) fn header_name_from_meta_key(key: &str) -> Option<String> {
    let lower = key.to_lowercase();
    if let Some(rest) = lower.strip_prefix("req.header.") {
        return Some(rest.to_string());
    }
    if let Some(rest) = lower.strip_prefix("resp.header.") {
        return Some(rest.to_string());
    }
    if lower == "resp.set_cookie" || lower.starts_with("resp.set_cookie.") {
        return Some("set-cookie".to_string());
    }
    None
}

/// Strip outbound meta entries whose header name is in the default sensitive
/// set plus `HeaderPolicy.masked`, unless explicitly in `HeaderPolicy.writable`.
/// Non-header meta entries pass through.
///
/// Stripped header names (deduped, lowercased) are appended to `stripped_names`.
pub(crate) fn sanitize_outbound_meta(
    meta: Vec<MetaEntry>,
    caps: &BlockCapabilities,
    stripped_names: &mut Vec<String>,
) -> Vec<MetaEntry> {
    meta.into_iter()
        .filter(|e| {
            let Some(name) = header_name_from_meta_key(&e.key) else {
                return true;
            };
            if !is_sensitive_header(&name, &caps.headers.masked) {
                return true;
            }
            let allowed = caps
                .headers
                .writable
                .iter()
                .any(|w| w.eq_ignore_ascii_case(&name));
            if !allowed {
                if !stripped_names.iter().any(|n| n == &name) {
                    stripped_names.push(name);
                }
                return false;
            }
            true
        })
        .collect()
}

/// Symmetric inbound sanitizer. Uses `HeaderPolicy.readable` as the allowlist.
pub(crate) fn sanitize_inbound_meta(
    meta: Vec<MetaEntry>,
    caps: &BlockCapabilities,
    stripped_names: &mut Vec<String>,
) -> Vec<MetaEntry> {
    meta.into_iter()
        .filter(|e| {
            let Some(name) = header_name_from_meta_key(&e.key) else {
                return true;
            };
            if !is_sensitive_header(&name, &caps.headers.masked) {
                return true;
            }
            let allowed = caps
                .headers
                .readable
                .iter()
                .any(|r| r.eq_ignore_ascii_case(&name));
            if !allowed {
                if !stripped_names.iter().any(|n| n == &name) {
                    stripped_names.push(name);
                }
                return false;
            }
            true
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Host-owned protected metadata namespace (SEC-01)
// ---------------------------------------------------------------------------

/// Whether `key` is in the host-owned protected metadata namespace.
///
/// Keys in this namespace carry authenticated identity / attribution that the
/// trusted host — or a trusted native block such as an auth middleware —
/// establishes. An untrusted WASM guest must never be able to forge, modify,
/// or remove them, because downstream authorization (e.g. the inspector block)
/// trusts them as identity. Currently the `auth.*` prefix, matched
/// case-insensitively (`auth.user_id`, `auth.user_roles`, …).
pub(crate) fn is_protected_meta_key(key: &str) -> bool {
    key.get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("auth."))
}

/// Snapshot the protected-namespace entries from `meta` (cloned) — the
/// host-provided identity for a request frame, captured before a guest runs
/// so it can be restored on the guest's outputs.
pub(crate) fn protected_meta_entries(meta: &[MetaEntry]) -> Vec<MetaEntry> {
    meta.iter()
        .filter(|e| is_protected_meta_key(&e.key))
        .cloned()
        .collect()
}

/// Enforce host ownership of the protected namespace on a value a guest
/// produced. Drops every protected key the guest set, then re-inserts the
/// inbound host-provided entries — so a guest can neither forge new identity
/// nor alter or strip the identity established upstream. Returns the
/// reconciled meta plus the distinct protected keys the guest attempted to set
/// (for warn-once logging).
pub(crate) fn restore_protected_meta(
    guest_meta: Vec<MetaEntry>,
    inbound_protected: &[MetaEntry],
) -> (Vec<MetaEntry>, Vec<String>) {
    let mut forged: Vec<String> = Vec::new();
    let mut out: Vec<MetaEntry> = guest_meta
        .into_iter()
        .filter(|e| {
            if is_protected_meta_key(&e.key) {
                if !forged.iter().any(|k| k == &e.key) {
                    forged.push(e.key.clone());
                }
                false
            } else {
                true
            }
        })
        .collect();
    out.extend(inbound_protected.iter().cloned());
    (out, forged)
}

#[cfg(test)]
mod header_name_tests {
    use super::header_name_from_meta_key;

    #[test]
    fn req_header_prefix() {
        assert_eq!(
            header_name_from_meta_key("req.header.authorization"),
            Some("authorization".to_string())
        );
    }

    #[test]
    fn req_header_uppercase_lowercased() {
        assert_eq!(
            header_name_from_meta_key("req.header.Authorization"),
            Some("authorization".to_string())
        );
    }

    #[test]
    fn resp_header_prefix() {
        assert_eq!(
            header_name_from_meta_key("resp.header.x-custom"),
            Some("x-custom".to_string())
        );
    }

    #[test]
    fn legacy_resp_set_cookie_bare() {
        assert_eq!(
            header_name_from_meta_key("resp.set_cookie"),
            Some("set-cookie".to_string())
        );
    }

    #[test]
    fn legacy_resp_set_cookie_nested() {
        assert_eq!(
            header_name_from_meta_key("resp.set_cookie.session"),
            Some("set-cookie".to_string())
        );
    }

    #[test]
    fn internal_meta_key_is_none() {
        assert_eq!(header_name_from_meta_key("auth.user_id"), None);
        assert_eq!(header_name_from_meta_key("trace_id"), None);
        assert_eq!(header_name_from_meta_key(""), None);
    }
}

#[cfg(test)]
mod sanitize_tests {
    use wafer_block::capabilities::{BlockCapabilities, HeaderPolicy};

    use super::*;

    fn meta(key: &str, value: &str) -> MetaEntry {
        MetaEntry {
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn outbound_strips_default_sensitive_when_empty_policy() {
        let caps = BlockCapabilities::default();
        let input = vec![
            meta("resp.header.content-type", "text/plain"),
            meta("resp.header.set-cookie", "s=1"),
            meta("resp.set_cookie", "legacy"),
            meta("resp.header.x-frame-options", "DENY"),
            meta("resp.header.x-safe", "ok"),
        ];
        let mut stripped = Vec::new();
        let out = sanitize_outbound_meta(input, &caps, &mut stripped);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"resp.header.content-type"));
        assert!(keys.contains(&"resp.header.x-safe"));
        assert!(!keys
            .iter()
            .any(|k| k.contains("set-cookie") || k.contains("set_cookie")));
        assert!(!keys.iter().any(|k| k.contains("x-frame-options")));
        assert!(stripped.contains(&"set-cookie".to_string()));
    }

    #[test]
    fn outbound_writable_allows_named_header() {
        let caps = BlockCapabilities {
            headers: HeaderPolicy {
                writable: vec!["set-cookie".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let input = vec![
            meta("resp.header.set-cookie", "s=1"),
            meta("resp.set_cookie", "legacy"),
            meta("resp.header.x-frame-options", "DENY"),
        ];
        let mut stripped = Vec::new();
        let out = sanitize_outbound_meta(input, &caps, &mut stripped);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"resp.header.set-cookie"));
        assert!(keys.contains(&"resp.set_cookie"));
        assert!(!keys.iter().any(|k| k.contains("x-frame-options")));
    }

    #[test]
    fn inbound_strips_default_sensitive_when_empty_policy() {
        let caps = BlockCapabilities::default();
        let input = vec![
            meta("req.header.accept", "text/plain"),
            meta("req.header.authorization", "Bearer abc"),
            meta("req.header.cookie", "a=1"),
        ];
        let mut stripped = Vec::new();
        let out = sanitize_inbound_meta(input, &caps, &mut stripped);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"req.header.accept"));
        assert!(!keys.iter().any(|k| k.contains("authorization")));
        assert!(!keys.iter().any(|k| k.contains("cookie")));
    }

    #[test]
    fn inbound_readable_allows_named_header() {
        let caps = BlockCapabilities {
            headers: HeaderPolicy {
                readable: vec!["authorization".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let input = vec![
            meta("req.header.authorization", "Bearer abc"),
            meta("req.header.cookie", "a=1"),
        ];
        let mut stripped = Vec::new();
        let out = sanitize_inbound_meta(input, &caps, &mut stripped);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"req.header.authorization"));
        assert!(!keys.iter().any(|k| k.contains("cookie")));
    }

    #[test]
    fn masked_extends_default_sensitive_both_directions() {
        let caps = BlockCapabilities {
            headers: HeaderPolicy {
                masked: vec!["x-internal".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let inbound = vec![meta("req.header.x-internal", "secret")];
        let outbound = vec![meta("resp.header.x-internal", "secret")];
        let mut s1 = Vec::new();
        let mut s2 = Vec::new();
        assert!(sanitize_inbound_meta(inbound, &caps, &mut s1).is_empty());
        assert!(sanitize_outbound_meta(outbound, &caps, &mut s2).is_empty());
    }

    #[test]
    fn non_header_keys_pass_through() {
        let caps = BlockCapabilities::default();
        let input = vec![meta("auth.user_id", "u1"), meta("trace_id", "abc")];
        let mut s = Vec::new();
        let out = sanitize_outbound_meta(input, &caps, &mut s);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"auth.user_id"));
        assert!(keys.contains(&"trace_id"));
    }

    // SEC-01: the host owns the `auth.*` namespace.
    use super::{is_protected_meta_key, restore_protected_meta};

    #[test]
    fn protected_key_matches_auth_prefix_case_insensitively() {
        assert!(is_protected_meta_key("auth.user_id"));
        assert!(is_protected_meta_key("auth.user_roles"));
        assert!(is_protected_meta_key("AUTH.User_Id"));
        assert!(!is_protected_meta_key("authorization"));
        assert!(!is_protected_meta_key("trace_id"));
        assert!(!is_protected_meta_key("au"));
    }

    #[test]
    fn restore_strips_guest_auth_and_restores_inbound_identity() {
        let inbound = vec![
            meta("auth.user_id", "alice"),
            meta("auth.user_roles", "user"),
        ];
        let guest = vec![
            meta("auth.user_id", "admin"),    // forged
            meta("auth.user_roles", "admin"), // forged
            meta("trace_id", "t1"),           // legitimate non-protected
        ];
        let (out, forged) = restore_protected_meta(guest, &inbound);
        let get = |k: &str| out.iter().find(|e| e.key == k).map(|e| e.value.as_str());
        assert_eq!(
            get("auth.user_id"),
            Some("alice"),
            "forged id replaced by inbound"
        );
        assert_eq!(
            get("auth.user_roles"),
            Some("user"),
            "forged roles replaced"
        );
        assert_eq!(get("trace_id"), Some("t1"), "non-protected meta preserved");
        assert_eq!(forged.len(), 2, "both forged keys reported");
    }

    #[test]
    fn restore_drops_guest_auth_when_no_inbound_identity() {
        // With no upstream identity, a guest cannot establish one.
        let guest = vec![meta("auth.user_roles", "admin"), meta("x", "y")];
        let (out, forged) = restore_protected_meta(guest, &[]);
        assert!(
            out.iter().all(|e| !is_protected_meta_key(&e.key)),
            "no auth.* survives from the guest"
        );
        assert!(
            out.iter().any(|e| e.key == "x"),
            "non-protected meta survives"
        );
        assert_eq!(forged, vec!["auth.user_roles".to_string()]);
    }
}
