//! Response headers that only get stricter as a flow's producers combine.
//!
//! [`super::response_meta::overlay`] lays a later producer's header over an
//! earlier one's, and for most headers the later value replaces the earlier.
//! For the headers here — every header the security-headers middleware sets
//! that restricts what a document may do, or what others may do with it —
//! the result is instead the stricter of the two, so a responding or failing
//! step, in this flow or in a `next` transfer's target, can tighten the
//! middleware's policy but never loosen it:
//!
//! - `Content-Security-Policy`: both policies. A header value may hold
//!   several comma-separated policies and a browser enforces every one, so
//!   the result admits only what both admit.
//! - `X-Frame-Options`: `DENY`, then `SAMEORIGIN`, then anything else.
//! - `X-Content-Type-Options`: `nosniff` over anything else.
//! - `Referrer-Policy`: the policy that sends less to another origin (see
//!   [`referrer_rank`]).
//! - `Strict-Transport-Security`: the longer `max-age`, then
//!   `includeSubDomains`.
//! - `Permissions-Policy`: per feature, the intersection of the two
//!   allowlists; a feature only one value names keeps that value's list.
//!
//! The result means the same whichever value came first; of two equally
//! strict values, the later stands. A value a browser would ignore (it does not
//! parse) never counts as stricter than one it applies.
//!
//! `Cross-Origin-Opener-Policy` and `Cross-Origin-Embedder-Policy` are not
//! listed: they opt a document into cross-origin isolation, and their values
//! are not ordered by strictness (`require-corp` refuses the cross-origin
//! subresources `credentialless` loads without credentials), so a page that
//! needs one mode sets it and the later value replaces the earlier, as for
//! any other header.

use std::cmp::Ordering;

/// The combination of an `earlier` and a `later` value of the header
/// `lower_name` (lower-case), or `None` when that header is not one of this
/// module's and the later value simply replaces the earlier.
pub(super) fn combine(lower_name: &str, earlier: &str, later: &str) -> Option<String> {
    let value = match lower_name {
        "content-security-policy" => csp_union(earlier, later),
        "x-frame-options" => stricter(earlier, later, x_frame_options_rank),
        "x-content-type-options" => stricter(earlier, later, is_nosniff),
        "referrer-policy" => stricter(earlier, later, referrer_rank),
        "strict-transport-security" => stricter(earlier, later, hsts_strength),
        "permissions-policy" => permissions_intersection(earlier, later),
        _ => return None,
    };
    Some(value)
}

/// `later` unless `earlier` ranks strictly higher.
fn stricter<K: Ord>(earlier: &str, later: &str, rank: impl Fn(&str) -> K) -> String {
    match rank(earlier).cmp(&rank(later)) {
        Ordering::Greater => earlier,
        Ordering::Less | Ordering::Equal => later,
    }
    .to_string()
}

/// Every policy of `earlier` then of `later`, each once, as one
/// comma-separated `Content-Security-Policy` value. A policy's ASCII
/// whitespace is collapsed so that the same policy written twice is kept
/// once; an empty policy (which a browser skips) is dropped.
fn csp_union(earlier: &str, later: &str) -> String {
    let mut policies: Vec<String> = Vec::new();
    for policy in earlier.split(',').chain(later.split(',')) {
        let policy = policy
            .split_ascii_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !policy.is_empty() && !policies.contains(&policy) {
            policies.push(policy);
        }
    }
    policies.join(", ")
}

/// How much an `X-Frame-Options` value forbids framing, as the HTML
/// standard reads it: `2` blocks every framing (`DENY`, or conflicting
/// values), `1` allows same-origin framing only (`SAMEORIGIN`), `0` is
/// ignored.
fn x_frame_options_rank(value: &str) -> u8 {
    let mut values: Vec<String> = Vec::new();
    for v in value.split(',').map(|v| v.trim().to_ascii_lowercase()) {
        if !values.contains(&v) {
            values.push(v);
        }
    }
    if values.len() > 1 {
        let blocks = values
            .iter()
            .any(|v| matches!(v.as_str(), "deny" | "allowall" | "sameorigin"));
        return if blocks { 2 } else { 0 };
    }
    match values.first().map(String::as_str) {
        Some("deny") => 2,
        Some("sameorigin") => 1,
        _ => 0,
    }
}

/// Whether a browser reads `X-Content-Type-Options` as `nosniff`: its first
/// comma-separated value is `nosniff`, in any case.
fn is_nosniff(value: &str) -> bool {
    value
        .split(',')
        .next()
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("nosniff"))
}

/// Referrer policies, the one that sends the least first: ordered by what a
/// cross-origin request receives (nothing; the origin, only over https; the
/// origin; the full URL, only over https; the full URL), then by what a
/// same-origin request receives.
const REFERRER_POLICIES: &[&str] = &[
    "no-referrer",
    "same-origin",
    "strict-origin",
    "strict-origin-when-cross-origin",
    "origin",
    "origin-when-cross-origin",
    "no-referrer-when-downgrade",
    "unsafe-url",
];

/// How little a `Referrer-Policy` value lets out: a browser applies the last
/// policy it recognises in the comma-separated list; the stricter that
/// policy, the higher the rank. `0` when it recognises none.
fn referrer_rank(value: &str) -> usize {
    value
        .split(',')
        .rev()
        .find_map(|token| {
            REFERRER_POLICIES
                .iter()
                .position(|p| p.eq_ignore_ascii_case(token.trim()))
        })
        .map_or(0, |i| REFERRER_POLICIES.len() - i)
}

/// A `Strict-Transport-Security` value's `(max-age, includeSubDomains)`, or
/// `None` when a browser ignores it (RFC 6797 §6.1: no valid `max-age`, or a
/// directive given twice).
fn hsts_strength(value: &str) -> Option<(u64, bool)> {
    let mut seen: Vec<String> = Vec::new();
    let mut max_age = None;
    let mut include_subdomains = false;
    for directive in value.split(';').map(str::trim).filter(|d| !d.is_empty()) {
        let (name, arg) = match directive.split_once('=') {
            Some((name, arg)) => (name.trim(), Some(arg.trim())),
            None => (directive, None),
        };
        let name = name.to_ascii_lowercase();
        if seen.contains(&name) {
            return None;
        }
        match name.as_str() {
            "max-age" => {
                let arg = arg?;
                let digits = arg
                    .strip_prefix('"')
                    .and_then(|a| a.strip_suffix('"'))
                    .unwrap_or(arg);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                max_age = Some(digits.parse().unwrap_or(u64::MAX));
            }
            "includesubdomains" => include_subdomains = true,
            _ => {}
        }
        seen.push(name);
    }
    max_age.map(|age| (age, include_subdomains))
}

/// The per-feature intersection of two `Permissions-Policy` values. A value
/// that is not a valid structured-field dictionary is ignored by a browser,
/// so the other value stands whole.
fn permissions_intersection(earlier: &str, later: &str) -> String {
    let (a, b) = match (parse_permissions(earlier), parse_permissions(later)) {
        (Some(a), Some(b)) => (a, b),
        (Some(_), None) => return earlier.to_string(),
        (None, _) => return later.to_string(),
    };
    let mut out: Vec<String> = Vec::new();
    for member in &a {
        match b.iter().find(|m| m.feature == member.feature) {
            Some(other) => out.push(member.intersect(other)),
            None => out.push(member.raw.clone()),
        }
    }
    for member in &b {
        if !a.iter().any(|m| m.feature == member.feature) {
            out.push(member.raw.clone());
        }
    }
    out.join(", ")
}

/// Who a feature is allowed for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Allowlist {
    /// `*`: every origin.
    All,
    /// The listed tokens (`self`, `src`) and origin strings, as written; empty
    /// is `()`, allowed for none.
    Only(Vec<String>),
}

/// One `feature=allowlist` member of a `Permissions-Policy` value.
#[derive(Debug)]
struct Permission {
    feature: String,
    /// The member as written, parameters included.
    raw: String,
    allowlist: Allowlist,
}

impl Permission {
    /// `feature=…` allowing only what both `self` and `other` allow.
    fn intersect(&self, other: &Permission) -> String {
        let allowlist = match (&self.allowlist, &other.allowlist) {
            (Allowlist::All, list) | (list, Allowlist::All) => list.clone(),
            (Allowlist::Only(a), Allowlist::Only(b)) => {
                Allowlist::Only(a.iter().filter(|item| b.contains(item)).cloned().collect())
            }
        };
        match allowlist {
            Allowlist::All => format!("{}=*", self.feature),
            Allowlist::Only(items) => format!("{}=({})", self.feature, items.join(" ")),
        }
    }
}

/// Parse a `Permissions-Policy` value as an RFC 8941 dictionary; `None` when
/// it is not one. A later member for a feature replaces an earlier one, as
/// in the structured-field parser; parameters are skipped.
fn parse_permissions(value: &str) -> Option<Vec<Permission>> {
    let s = value.as_bytes();
    let mut i = skip(s, 0, b" \t");
    let mut out: Vec<Permission> = Vec::new();
    while i < s.len() {
        let start = i;
        let feature = parse_key(s, &mut i)?;
        let items = if s.get(i) == Some(&b'=') {
            i += 1;
            if s.get(i) == Some(&b'(') {
                parse_inner_list(s, &mut i)?
            } else {
                let item = parse_bare_item(s, &mut i)?;
                parse_params(s, &mut i)?;
                vec![item]
            }
        } else {
            // A bare key is the boolean `true`, which names no origin.
            parse_params(s, &mut i)?;
            Vec::new()
        };
        let allowlist = if items.iter().any(|item| item == "*") {
            Allowlist::All
        } else {
            Allowlist::Only(items)
        };
        let member = Permission {
            feature,
            raw: value[start..i].to_string(),
            allowlist,
        };
        match out.iter_mut().find(|m| m.feature == member.feature) {
            Some(existing) => *existing = member,
            None => out.push(member),
        }
        i = skip(s, i, b" \t");
        if i == s.len() {
            break;
        }
        if s[i] != b',' {
            return None;
        }
        i = skip(s, i + 1, b" \t");
        if i == s.len() {
            return None;
        }
    }
    Some(out)
}

fn skip(s: &[u8], mut i: usize, set: &[u8]) -> usize {
    while s.get(i).is_some_and(|b| set.contains(b)) {
        i += 1;
    }
    i
}

/// `key = ( lcalpha / "*" ) *( lcalpha / DIGIT / "_" / "-" / "." / "*" )`.
fn parse_key(s: &[u8], i: &mut usize) -> Option<String> {
    let start = *i;
    if !s
        .get(start)
        .is_some_and(|b| b.is_ascii_lowercase() || *b == b'*')
    {
        return None;
    }
    *i += 1;
    while s
        .get(*i)
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.*".contains(b))
    {
        *i += 1;
    }
    Some(String::from_utf8_lossy(&s[start..*i]).into_owned())
}

/// `inner-list = "(" *SP [ item *( 1*SP item ) *SP ] ")" parameters`; the
/// tokens and strings among its items (a browser ignores any other item).
fn parse_inner_list(s: &[u8], i: &mut usize) -> Option<Vec<String>> {
    *i += 1;
    let mut items = Vec::new();
    loop {
        *i = skip(s, *i, b" ");
        match s.get(*i)? {
            b')' => {
                *i += 1;
                parse_params(s, i)?;
                return Some(items);
            }
            _ => {
                let item = parse_bare_item(s, i)?;
                parse_params(s, i)?;
                if !matches!(s.get(*i), Some(b' ' | b')')) {
                    return None;
                }
                items.push(item);
            }
        }
    }
}

/// `parameters = *( ";" *SP key [ "=" bare-item ] )`, skipped.
fn parse_params(s: &[u8], i: &mut usize) -> Option<()> {
    while s.get(*i) == Some(&b';') {
        *i = skip(s, *i + 1, b" ");
        parse_key(s, i)?;
        if s.get(*i) == Some(&b'=') {
            *i += 1;
            parse_bare_item(s, i)?;
        }
    }
    Some(())
}

/// One bare item, returned as written: a string (quotes included), token,
/// number, boolean or byte sequence.
fn parse_bare_item(s: &[u8], i: &mut usize) -> Option<String> {
    let start = *i;
    match *s.get(start)? {
        b'"' => {
            *i += 1;
            loop {
                match *s.get(*i)? {
                    b'\\' => {
                        if !matches!(s.get(*i + 1), Some(b'"' | b'\\')) {
                            return None;
                        }
                        *i += 2;
                    }
                    b'"' => {
                        *i += 1;
                        break;
                    }
                    b if (0x20..=0x7e).contains(&b) => *i += 1,
                    _ => return None,
                }
            }
        }
        b if b.is_ascii_alphabetic() || b == b'*' => {
            *i += 1;
            while s
                .get(*i)
                .is_some_and(|b| is_tchar(*b) || matches!(b, b':' | b'/'))
            {
                *i += 1;
            }
        }
        b if b.is_ascii_digit() || b == b'-' => {
            *i += 1;
            while s.get(*i).is_some_and(|b| b.is_ascii_digit() || *b == b'.') {
                *i += 1;
            }
        }
        b'?' => {
            if !matches!(s.get(start + 1), Some(b'0' | b'1')) {
                return None;
            }
            *i += 2;
        }
        b':' => {
            *i += 1;
            while s
                .get(*i)
                .is_some_and(|b| b.is_ascii_alphanumeric() || b"+/=".contains(b))
            {
                *i += 1;
            }
            if s.get(*i) != Some(&b':') {
                return None;
            }
            *i += 1;
        }
        _ => return None,
    }
    Some(String::from_utf8_lossy(&s[start..*i]).into_owned())
}

/// RFC 9110 `tchar`.
fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `combine` both ways round: the result must not depend on the order.
    fn both_ways(name: &str, a: &str, b: &str) -> (String, String) {
        (
            combine(name, a, b).expect("a restrictive header"),
            combine(name, b, a).expect("a restrictive header"),
        )
    }

    #[test]
    fn other_headers_are_not_combined() {
        assert_eq!(combine("cache-control", "no-store", "max-age=60"), None);
        assert_eq!(
            combine(
                "cross-origin-embedder-policy",
                "require-corp",
                "credentialless"
            ),
            None
        );
    }

    #[test]
    fn csp_keeps_both_policies_once() {
        let (ab, ba) = both_ways(
            "content-security-policy",
            "default-src 'self'; frame-ancestors 'none'",
            "default-src *",
        );
        assert_eq!(
            ab,
            "default-src 'self'; frame-ancestors 'none', default-src *"
        );
        assert_eq!(
            ba,
            "default-src *, default-src 'self'; frame-ancestors 'none'"
        );

        let same = combine(
            "content-security-policy",
            "default-src 'self'",
            " default-src  'self' , ",
        );
        assert_eq!(same.as_deref(), Some("default-src 'self'"));
    }

    #[test]
    fn x_frame_options_deny_beats_sameorigin_beats_anything_else() {
        for (a, b, want) in [
            ("DENY", "SAMEORIGIN", "DENY"),
            ("SAMEORIGIN", "ALLOWALL", "SAMEORIGIN"),
            ("SAMEORIGIN", "ALLOW-FROM https://a.example", "SAMEORIGIN"),
            ("SAMEORIGIN", "deny", "deny"),
            // Conflicting values block every framing, like `DENY`.
            ("SAMEORIGIN", "DENY, SAMEORIGIN", "DENY, SAMEORIGIN"),
        ] {
            let (ab, ba) = both_ways("x-frame-options", a, b);
            assert_eq!((ab.as_str(), ba.as_str()), (want, want), "{a} / {b}");
        }
    }

    #[test]
    fn nosniff_is_kept() {
        for other in ["", "sniff", "garbage, nosniff"] {
            let (ab, ba) = both_ways("x-content-type-options", "nosniff", other);
            assert_eq!(
                (ab.as_str(), ba.as_str()),
                ("nosniff", "nosniff"),
                "{other}"
            );
        }
    }

    #[test]
    fn referrer_policy_keeps_the_one_that_sends_less() {
        for (a, b, want) in [
            (
                "strict-origin-when-cross-origin",
                "unsafe-url",
                "strict-origin-when-cross-origin",
            ),
            (
                "strict-origin-when-cross-origin",
                "no-referrer",
                "no-referrer",
            ),
            (
                "strict-origin-when-cross-origin",
                "origin",
                "strict-origin-when-cross-origin",
            ),
            ("same-origin", "strict-origin", "same-origin"),
            // The last recognised token is the one a browser applies.
            ("origin", "no-referrer, unsafe-url", "origin"),
            ("origin", "unsafe-url, bogus", "origin"),
            ("unsafe-url", "bogus", "unsafe-url"),
        ] {
            let (ab, ba) = both_ways("referrer-policy", a, b);
            assert_eq!((ab.as_str(), ba.as_str()), (want, want), "{a} / {b}");
        }
    }

    #[test]
    fn hsts_keeps_the_longer_max_age_then_subdomains() {
        let full = "max-age=31536000; includeSubDomains; preload";
        for (other, want) in [
            ("max-age=0", full),
            ("max-age=600", full),
            ("max-age=31536000", full),
            ("includeSubDomains", full),
            ("max-age=1; max-age=63072000", full),
            ("max-age=\"63072000\"", "max-age=\"63072000\""),
        ] {
            let (ab, ba) = both_ways("strict-transport-security", full, other);
            assert_eq!((ab.as_str(), ba.as_str()), (want, want), "{other}");
        }
    }

    #[test]
    fn permissions_policy_intersects_each_feature() {
        let middleware = "camera=(), microphone=(), geolocation=()";
        let (ab, ba) = both_ways(
            "permissions-policy",
            middleware,
            "camera=*, geolocation=(self), fullscreen=(self)",
        );
        assert_eq!(
            ab,
            "camera=(), microphone=(), geolocation=(), fullscreen=(self)"
        );
        assert_eq!(
            ba,
            "camera=(), geolocation=(), fullscreen=(self), microphone=()"
        );

        let narrowed = combine(
            "permissions-policy",
            "fullscreen=*, usb=(self \"https://a.example\")",
            "fullscreen=(self);report-to=r, usb=(\"https://a.example\" \"https://b.example\")",
        );
        assert_eq!(
            narrowed.as_deref(),
            Some("fullscreen=(self), usb=(\"https://a.example\")")
        );
    }

    #[test]
    fn an_unparseable_permissions_policy_leaves_the_other_whole() {
        for bad in ["camera=*,", "Camera=*", "camera=(self", "camera=\"x"] {
            let (ab, ba) = both_ways("permissions-policy", "camera=()", bad);
            assert_eq!(
                (ab.as_str(), ba.as_str()),
                ("camera=()", "camera=()"),
                "{bad}"
            );
        }
    }
}
