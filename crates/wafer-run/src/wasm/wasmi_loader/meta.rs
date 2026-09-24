//! Guest meta sanitisation at the WASM guest boundary, shared by the
//! host-import linker and the [`super::WasmiBlock`] dispatch.
//!
//! Every meta entry that carries an HTTP header — a request header as the
//! HTTP codec writes it ([`META_HTTP_HEADER_PREFIX`]), a response header
//! ([`META_RESP_HEADER_PREFIX`]) or a `Set-Cookie` directive
//! ([`META_RESP_COOKIE_PREFIX`]) — is subject to the block's `HeaderPolicy`:
//!
//! A header is sensitive when it is in [`DEFAULT_SENSITIVE_HEADERS`] or in
//! `HeaderPolicy.masked`.
//!
//! - **Into the guest** ([`prepare_guest_inbound`]): a sensitive header
//!   reaches the guest only if its name is in `HeaderPolicy.readable`.
//! - **Out of the guest** ([`sanitize_guest_egress`]): a sensitive header
//!   leaves the guest only if its name is in `HeaderPolicy.writable`. This
//!   holds for every guest egress — a `Respond` result's meta, an `Error`
//!   result's meta, a `Continue` message handed to the next flow step, and
//!   the message of a nested `call_block`.
//!
//! The host also owns two things the guest can neither forge nor remove: the
//! authenticated identity in the `auth.*` namespace, and — on a `Continue`
//! message — the inbound value of every sensitive header the guest may not
//! write. Both are captured in [`HostOwnedMeta`] before the guest runs.

use wafer_block::{
    capabilities::DEFAULT_SENSITIVE_HEADERS,
    core_types::*,
    http_codec::META_HTTP_HEADER_PREFIX,
    meta::{META_RESP_COOKIE_PREFIX, META_RESP_HEADER_PREFIX},
};

use crate::wasm::capabilities::BlockCapabilities;

// ---------------------------------------------------------------------------
// Header-derived meta keys
// ---------------------------------------------------------------------------

fn is_sensitive_header(name: &str, policy_masked: &[String]) -> bool {
    DEFAULT_SENSITIVE_HEADERS.contains(&name)
        || policy_masked.iter().any(|m| m.eq_ignore_ascii_case(name))
}

fn is_listed(list: &[String], name: &str) -> bool {
    list.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// Extract the canonical (lowercase) HTTP header name from a wafer meta key,
/// or `None` if the key is not a header.
///
/// Matches the three header-carrying key families, case-insensitively:
/// - [`META_HTTP_HEADER_PREFIX`]`{name}` — inbound request header, as
///   [`wafer_block::http_codec::build_http_message`] writes it
/// - [`META_RESP_HEADER_PREFIX`]`{name}` — outbound response header
/// - [`META_RESP_COOKIE_PREFIX`]`*` — one `Set-Cookie` directive, mapped to
///   `set-cookie`
pub(crate) fn header_name_from_meta_key(key: &str) -> Option<String> {
    let lower = key.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix(META_HTTP_HEADER_PREFIX) {
        return Some(rest.to_string());
    }
    if let Some(rest) = lower.strip_prefix(META_RESP_HEADER_PREFIX) {
        return Some(rest.to_string());
    }
    if lower.starts_with(META_RESP_COOKIE_PREFIX) {
        return Some("set-cookie".to_string());
    }
    None
}

/// The sensitive header `key` carries under `caps` (default set plus
/// `HeaderPolicy.masked`), or `None` for a non-header or non-sensitive key.
fn sensitive_header_name(key: &str, caps: &BlockCapabilities) -> Option<String> {
    header_name_from_meta_key(key).filter(|name| is_sensitive_header(name, &caps.headers.masked))
}

fn push_distinct(names: &mut Vec<String>, name: String) {
    if !names.contains(&name) {
        names.push(name);
    }
}

// ---------------------------------------------------------------------------
// Into the guest
// ---------------------------------------------------------------------------

/// Meta the host owns for one guest invocation, captured from the inbound
/// message before the guest sees it. See [`sanitize_guest_egress`] for how
/// each part is restored.
#[derive(Debug, Clone, Default)]
pub(crate) struct HostOwnedMeta {
    /// The protected `auth.*` entries (SEC-01): the identity established
    /// upstream by the trusted host / auth middleware.
    identity: Vec<MetaEntry>,
    /// Every inbound entry carrying a sensitive header, whether or not the
    /// guest may read it.
    sensitive_headers: Vec<MetaEntry>,
}

/// The inbound message meta split for a guest invocation.
pub(crate) struct GuestInbound {
    /// What the guest is handed: the inbound meta minus every sensitive
    /// header outside `HeaderPolicy.readable`.
    pub(crate) meta: Vec<MetaEntry>,
    /// Distinct names of the sensitive headers withheld from the guest.
    pub(crate) withheld: Vec<String>,
    /// What the host restores on the guest's egress.
    pub(crate) host_owned: HostOwnedMeta,
}

/// Split inbound `meta` into what the guest may see and what the host keeps.
/// A sensitive header (the default set plus `HeaderPolicy.masked`) reaches
/// the guest only if its name is in `HeaderPolicy.readable`; every other
/// entry passes through.
pub(crate) fn prepare_guest_inbound(
    meta: Vec<MetaEntry>,
    caps: &BlockCapabilities,
) -> GuestInbound {
    let host_owned = HostOwnedMeta {
        identity: meta
            .iter()
            .filter(|e| is_protected_meta_key(&e.key))
            .cloned()
            .collect(),
        sensitive_headers: meta
            .iter()
            .filter(|e| sensitive_header_name(&e.key, caps).is_some())
            .cloned()
            .collect(),
    };
    let mut withheld = Vec::new();
    let meta = meta
        .into_iter()
        .filter(|e| match sensitive_header_name(&e.key, caps) {
            Some(name) if !is_listed(&caps.headers.readable, &name) => {
                push_distinct(&mut withheld, name);
                false
            }
            _ => true,
        })
        .collect();
    GuestInbound {
        meta,
        withheld,
        host_owned,
    }
}

// ---------------------------------------------------------------------------
// Out of the guest
// ---------------------------------------------------------------------------

/// Where a guest-produced meta set is going. Every egress gets the same
/// header allowlist and identity restore; they differ only in whether the
/// host's inbound sensitive headers are put back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuestEgress {
    /// The meta of a `Respond` result.
    Respond,
    /// The meta of an `Error` result.
    Error,
    /// The message a `Continue` result hands to the next flow step. It
    /// REPLACES the flow's message, so the host restores the inbound value of
    /// every sensitive header the guest may not write — a guest that could
    /// not see a request's `Cookie` must not be able to drop it, or change
    /// it, for the steps after it.
    Continue,
    /// The message of a nested `call_block` the guest initiates. A fresh
    /// request the guest composed: nothing of the inbound request's headers
    /// is added to it.
    Call,
}

/// A guest egress after [`sanitize_guest_egress`].
pub(crate) struct SanitizedEgress {
    /// The meta the host forwards.
    pub(crate) meta: Vec<MetaEntry>,
    /// Distinct names of the sensitive headers the guest emitted without
    /// holding them in `HeaderPolicy.writable` (dropped).
    pub(crate) stripped: Vec<String>,
    /// Distinct protected (`auth.*`) keys the guest tried to set (dropped).
    pub(crate) forged: Vec<String>,
}

/// Apply the guest-egress policy to meta a guest produced:
///
/// 1. drop every sensitive header (default set plus `HeaderPolicy.masked`)
///    whose name is not in `HeaderPolicy.writable`;
/// 2. drop every protected `auth.*` key the guest set and re-insert the
///    host-provided identity, so a guest can neither forge identity nor
///    alter or strip the identity established upstream (SEC-01);
/// 3. on [`GuestEgress::Continue`] only, re-insert the inbound entries for
///    every sensitive header the guest may not write.
pub(crate) fn sanitize_guest_egress(
    meta: Vec<MetaEntry>,
    caps: &BlockCapabilities,
    host_owned: &HostOwnedMeta,
    egress: GuestEgress,
) -> SanitizedEgress {
    let writable = &caps.headers.writable;
    let mut stripped = Vec::new();
    let mut forged = Vec::new();
    let mut out: Vec<MetaEntry> = meta
        .into_iter()
        .filter(|e| {
            if is_protected_meta_key(&e.key) {
                push_distinct(&mut forged, e.key.clone());
                return false;
            }
            match sensitive_header_name(&e.key, caps) {
                Some(name) if !is_listed(writable, &name) => {
                    // An inbound entry handed back unchanged (a readable
                    // header the guest passed through) is dropped too, but
                    // it is not an attempt to write the header.
                    if !host_owned.sensitive_headers.contains(e) {
                        push_distinct(&mut stripped, name);
                    }
                    false
                }
                _ => true,
            }
        })
        .collect();
    out.extend(host_owned.identity.iter().cloned());
    match egress {
        GuestEgress::Continue => out.extend(
            host_owned
                .sensitive_headers
                .iter()
                .filter(|e| {
                    sensitive_header_name(&e.key, caps)
                        .is_some_and(|name| !is_listed(writable, &name))
                })
                .cloned(),
        ),
        GuestEgress::Respond | GuestEgress::Error | GuestEgress::Call => {}
    }
    SanitizedEgress {
        meta: out,
        stripped,
        forged,
    }
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

#[cfg(test)]
mod header_name_tests {
    use super::header_name_from_meta_key;

    #[test]
    fn http_header_prefix_is_a_request_header() {
        assert_eq!(
            header_name_from_meta_key("http.header.authorization"),
            Some("authorization".to_string())
        );
    }

    #[test]
    fn key_prefix_and_name_are_matched_case_insensitively() {
        assert_eq!(
            header_name_from_meta_key("HTTP.Header.Authorization"),
            Some("authorization".to_string())
        );
        assert_eq!(
            header_name_from_meta_key("resp.header.Set-Cookie"),
            Some("set-cookie".to_string())
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
    fn resp_set_cookie_prefix_is_set_cookie() {
        assert_eq!(
            header_name_from_meta_key("resp.set_cookie.session"),
            Some("set-cookie".to_string())
        );
    }

    #[test]
    fn internal_meta_key_is_none() {
        assert_eq!(header_name_from_meta_key("auth.user_id"), None);
        assert_eq!(header_name_from_meta_key("trace_id"), None);
        assert_eq!(header_name_from_meta_key("http.path"), None);
        assert_eq!(header_name_from_meta_key(""), None);
    }
}

#[cfg(test)]
mod sanitize_tests {
    use wafer_block::{
        capabilities::{BlockCapabilities, HeaderPolicy},
        http_codec::build_http_message,
    };

    use super::*;

    fn meta(key: &str, value: &str) -> MetaEntry {
        MetaEntry {
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    fn keys(meta: &[MetaEntry]) -> Vec<&str> {
        meta.iter().map(|e| e.key.as_str()).collect()
    }

    fn get<'a>(meta: &'a [MetaEntry], key: &str) -> Option<&'a str> {
        meta.iter().find(|e| e.key == key).map(|e| e.value.as_str())
    }

    fn with_headers(policy: HeaderPolicy) -> BlockCapabilities {
        BlockCapabilities {
            headers: policy,
            ..BlockCapabilities::none()
        }
    }

    /// The request meta exactly as the HTTP codec produces it for a request
    /// carrying a session cookie and a bearer token.
    fn codec_request_meta() -> Vec<MetaEntry> {
        build_http_message(
            "GET",
            "/b/guest/",
            "",
            "127.0.0.1",
            [
                ("Cookie", "s=1"),
                ("Authorization", "Bearer x"),
                ("Accept", "text/html"),
            ],
        )
        .meta
    }

    #[test]
    fn inbound_withholds_codec_cookie_and_authorization_by_default() {
        let inbound = prepare_guest_inbound(codec_request_meta(), &BlockCapabilities::none());
        assert_eq!(get(&inbound.meta, "http.header.cookie"), None);
        assert_eq!(get(&inbound.meta, "http.header.authorization"), None);
        assert_eq!(get(&inbound.meta, "http.header.accept"), Some("text/html"));
        assert_eq!(get(&inbound.meta, "http.path"), Some("/b/guest/"));
        assert_eq!(inbound.withheld, vec!["cookie", "authorization"]);
    }

    #[test]
    fn inbound_readable_declaration_admits_only_the_named_header() {
        let caps = with_headers(HeaderPolicy {
            readable: vec!["Authorization".into()],
            ..Default::default()
        });
        let inbound = prepare_guest_inbound(codec_request_meta(), &caps);
        assert_eq!(
            get(&inbound.meta, "http.header.authorization"),
            Some("Bearer x")
        );
        assert_eq!(get(&inbound.meta, "http.header.cookie"), None);
        assert_eq!(inbound.withheld, vec!["cookie"]);
    }

    #[test]
    fn masked_extends_default_sensitive_both_directions() {
        let caps = with_headers(HeaderPolicy {
            masked: vec!["x-internal".into()],
            ..Default::default()
        });
        let inbound = prepare_guest_inbound(vec![meta("http.header.x-internal", "secret")], &caps);
        assert!(inbound.meta.is_empty());
        let out = sanitize_guest_egress(
            vec![meta("resp.header.x-internal", "secret")],
            &caps,
            &HostOwnedMeta::default(),
            GuestEgress::Respond,
        );
        assert!(out.meta.is_empty());
        assert_eq!(out.stripped, vec!["x-internal"]);
    }

    /// Guest-produced response meta that sets a cookie, redirects, widens
    /// CORS and forges a request header for the next step.
    fn hostile_egress_meta() -> Vec<MetaEntry> {
        vec![
            meta("resp.header.content-type", "text/plain"),
            meta("resp.header.x-safe", "ok"),
            meta("resp.set_cookie.s", "s=evil"),
            meta("resp.header.Set-Cookie", "t=evil"),
            meta("resp.header.location", "https://evil.example/"),
            meta("resp.header.access-control-allow-origin", "*"),
            meta("http.header.authorization", "Bearer forged"),
            meta("trace_id", "t1"),
        ]
    }

    #[test]
    fn every_egress_strips_sensitive_headers_outside_writable() {
        for egress in [
            GuestEgress::Respond,
            GuestEgress::Error,
            GuestEgress::Continue,
            GuestEgress::Call,
        ] {
            let out = sanitize_guest_egress(
                hostile_egress_meta(),
                &BlockCapabilities::none(),
                &HostOwnedMeta::default(),
                egress,
            );
            assert_eq!(
                keys(&out.meta),
                vec!["resp.header.content-type", "resp.header.x-safe", "trace_id"],
                "{egress:?}"
            );
            assert_eq!(
                out.stripped,
                vec![
                    "set-cookie",
                    "location",
                    "access-control-allow-origin",
                    "authorization"
                ],
                "{egress:?}"
            );
        }
    }

    #[test]
    fn refresh_and_clear_site_data_are_withheld_on_every_egress() {
        // `Refresh: 0;url=…` navigates exactly like `Location`, and
        // `Clear-Site-Data` wipes the origin's cookies and storage (it logs the
        // user out); neither is the guest's to set.
        for egress in [
            GuestEgress::Respond,
            GuestEgress::Error,
            GuestEgress::Continue,
            GuestEgress::Call,
        ] {
            let out = sanitize_guest_egress(
                vec![
                    meta("resp.header.Refresh", "0;url=https://evil.example/"),
                    meta("resp.header.clear-site-data", "\"cookies\", \"storage\""),
                ],
                &BlockCapabilities::none(),
                &HostOwnedMeta::default(),
                egress,
            );
            assert!(out.meta.is_empty(), "{egress:?}: {:?}", out.meta);
            assert_eq!(
                out.stripped,
                vec!["refresh", "clear-site-data"],
                "{egress:?}"
            );
        }
    }

    #[test]
    fn inbound_withholds_proxy_authorization() {
        let msg = build_http_message(
            "GET",
            "/b/guest/",
            "",
            "127.0.0.1",
            [("Proxy-Authorization", "Basic cHJveHk6c2VjcmV0")],
        );
        let inbound = prepare_guest_inbound(msg.meta, &BlockCapabilities::none());
        assert_eq!(get(&inbound.meta, "http.header.proxy-authorization"), None);
        assert_eq!(inbound.withheld, vec!["proxy-authorization"]);
    }

    #[test]
    fn writable_declaration_admits_only_the_named_header() {
        let caps = with_headers(HeaderPolicy {
            writable: vec!["set-cookie".into()],
            ..Default::default()
        });
        let out = sanitize_guest_egress(
            hostile_egress_meta(),
            &caps,
            &HostOwnedMeta::default(),
            GuestEgress::Error,
        );
        assert_eq!(get(&out.meta, "resp.set_cookie.s"), Some("s=evil"));
        assert_eq!(get(&out.meta, "resp.header.Set-Cookie"), Some("t=evil"));
        assert_eq!(get(&out.meta, "resp.header.location"), None);
    }

    #[test]
    fn continue_restores_the_inbound_headers_the_guest_may_not_write() {
        let caps = with_headers(HeaderPolicy {
            readable: vec!["authorization".into()],
            ..Default::default()
        });
        let inbound = prepare_guest_inbound(codec_request_meta(), &caps);
        // The guest passes its (sanitized) message on after forging a cookie
        // and rewriting the authorization it was allowed to read.
        let mut guest_meta = inbound.meta.clone();
        guest_meta.retain(|e| e.key != "http.header.authorization");
        guest_meta.push(meta("http.header.cookie", "s=forged"));
        guest_meta.push(meta("http.header.authorization", "Bearer forged"));

        let out = sanitize_guest_egress(
            guest_meta,
            &caps,
            &inbound.host_owned,
            GuestEgress::Continue,
        );
        assert_eq!(get(&out.meta, "http.header.cookie"), Some("s=1"));
        assert_eq!(
            get(&out.meta, "http.header.authorization"),
            Some("Bearer x")
        );
        assert_eq!(
            out.meta
                .iter()
                .filter(|e| e.key.starts_with("http.header."))
                .count(),
            3,
            "each header exactly once: {:?}",
            out.meta
        );
        assert_eq!(out.stripped, vec!["cookie", "authorization"]);
    }

    #[test]
    fn a_readable_header_passed_through_is_not_reported_as_stripped() {
        let caps = with_headers(HeaderPolicy {
            readable: vec!["authorization".into()],
            ..Default::default()
        });
        let inbound = prepare_guest_inbound(codec_request_meta(), &caps);
        let out = sanitize_guest_egress(
            inbound.meta.clone(),
            &caps,
            &inbound.host_owned,
            GuestEgress::Continue,
        );
        assert!(out.stripped.is_empty(), "{:?}", out.stripped);
        assert_eq!(
            get(&out.meta, "http.header.authorization"),
            Some("Bearer x")
        );
    }

    #[test]
    fn only_continue_restores_inbound_headers() {
        let inbound = prepare_guest_inbound(codec_request_meta(), &BlockCapabilities::none());
        for egress in [GuestEgress::Respond, GuestEgress::Error, GuestEgress::Call] {
            let out = sanitize_guest_egress(
                Vec::new(),
                &BlockCapabilities::none(),
                &inbound.host_owned,
                egress,
            );
            assert!(out.meta.is_empty(), "{egress:?}: {:?}", out.meta);
        }
    }

    #[test]
    fn non_header_keys_pass_through() {
        let out = sanitize_guest_egress(
            vec![meta("trace_id", "abc"), meta("http.path", "/x")],
            &BlockCapabilities::none(),
            &HostOwnedMeta::default(),
            GuestEgress::Respond,
        );
        assert_eq!(keys(&out.meta), vec!["trace_id", "http.path"]);
    }

    // SEC-01: the host owns the `auth.*` namespace.

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
    fn egress_strips_guest_auth_and_restores_inbound_identity() {
        let inbound = prepare_guest_inbound(
            vec![
                meta("auth.user_id", "alice"),
                meta("auth.user_roles", "user"),
            ],
            &BlockCapabilities::none(),
        );
        for egress in [
            GuestEgress::Respond,
            GuestEgress::Error,
            GuestEgress::Continue,
            GuestEgress::Call,
        ] {
            let guest = vec![
                meta("auth.user_id", "admin"),    // forged
                meta("auth.user_roles", "admin"), // forged
                meta("trace_id", "t1"),           // legitimate non-protected
            ];
            let out = sanitize_guest_egress(
                guest,
                &BlockCapabilities::none(),
                &inbound.host_owned,
                egress,
            );
            assert_eq!(get(&out.meta, "auth.user_id"), Some("alice"), "{egress:?}");
            assert_eq!(
                get(&out.meta, "auth.user_roles"),
                Some("user"),
                "{egress:?}"
            );
            assert_eq!(get(&out.meta, "trace_id"), Some("t1"), "{egress:?}");
            assert_eq!(out.forged.len(), 2, "{egress:?}");
        }
    }

    #[test]
    fn egress_drops_guest_auth_when_no_inbound_identity() {
        // With no upstream identity, a guest cannot establish one.
        let out = sanitize_guest_egress(
            vec![meta("auth.user_roles", "admin"), meta("x", "y")],
            &BlockCapabilities::none(),
            &HostOwnedMeta::default(),
            GuestEgress::Respond,
        );
        assert_eq!(keys(&out.meta), vec!["x"]);
        assert_eq!(out.forged, vec!["auth.user_roles".to_string()]);
    }
}
