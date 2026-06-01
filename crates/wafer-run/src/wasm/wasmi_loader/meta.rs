//! Guest meta (HTTP header) sanitisation shared by the host-import linker
//! and the [`super::WasmiBlock`] dispatch. WASM blocks may only read/write
//! header-derived meta keys they have been granted via `HeaderPolicy`.

use crate::{types::*, wasm::capabilities::BlockCapabilities};

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
}
