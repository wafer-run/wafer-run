//! How response meta combines inside a flow: a responding step's meta laid
//! over the flow message ([`overlay`]), and the middleware headers a
//! short-circuit terminal inherits ([`carried`]). See the executor's module
//! docs for where each applies.
//!
//! Identity rules, shared by both:
//! - a header is identified by its name, case-insensitively — except the
//!   list-valued [`UNION_HEADERS`], whose values are unioned (so a `Vary:
//!   Origin` a CORS middleware set survives a terminal's own `Vary`);
//! - a cookie is identified by its name plus its `Path` and `Domain`
//!   attributes (RFC 6265 §5.3 step 11), not by its `resp.set_cookie.*` key:
//!   [`wafer_block::response::ResponseBuilder`] keys cookies by position
//!   (`resp.set_cookie.0`, `.1`, …), so two producers' keys collide for
//!   unrelated cookies;
//! - any other key is identified by the key itself.

use std::collections::HashSet;

use wafer_block::{
    core_types::MetaEntry,
    http_codec::{classify_response_meta, ResponseMetaPart},
    meta::META_RESP_COOKIE_PREFIX,
};

/// Headers that describe a body (or a redirect to one), never carried from
/// the flow message onto a short-circuit terminal: the terminal has its own
/// body.
const BODY_HEADERS: &[&str] = &[
    "content-type",
    "content-encoding",
    "content-length",
    "content-disposition",
    "content-language",
    "content-location",
    "content-range",
    "accept-ranges",
    "etag",
    "last-modified",
    "location",
];

/// List-valued headers whose values [`overlay`] unions instead of replacing.
const UNION_HEADERS: &[&str] = &["vary"];

fn is_listed(list: &[&str], name: &str) -> bool {
    list.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// A `Set-Cookie` directive's identity: name, `Path`, `Domain` (attribute
/// values compared case-insensitively, the name exactly).
#[derive(PartialEq, Eq)]
struct CookieId {
    name: String,
    path: String,
    domain: String,
}

fn cookie_id(directive: &str) -> CookieId {
    let mut parts = directive.split(';');
    let name = parts
        .next()
        .and_then(|pair| pair.split('=').next())
        .unwrap_or("")
        .trim()
        .to_string();
    let mut path = String::new();
    let mut domain = String::new();
    for attr in parts {
        let (key, value) = attr.split_once('=').unwrap_or((attr, ""));
        let value = value.trim().to_ascii_lowercase();
        if key.trim().eq_ignore_ascii_case("path") {
            path = value;
        } else if key.trim().eq_ignore_ascii_case("domain") {
            domain = value.trim_start_matches('.').to_string();
        }
    }
    CookieId { name, path, domain }
}

/// What a meta entry is, for overlay purposes.
enum Kind {
    Cookie,
    Header { name: String },
    Other,
}

fn kind(entry: &MetaEntry) -> Kind {
    match classify_response_meta(entry) {
        Some(ResponseMetaPart::SetCookie(_)) => Kind::Cookie,
        Some(ResponseMetaPart::Header { name, .. }) => Kind::Header {
            name: name.to_ascii_lowercase(),
        },
        Some(ResponseMetaPart::Status(_) | ResponseMetaPart::ContentType(_)) | None => Kind::Other,
    }
}

fn header_name_is(entry: &MetaEntry, lower_name: &str) -> bool {
    matches!(kind(entry), Kind::Header { name } if name == lower_name)
}

/// The comma-separated tokens of `values`, in order, each once
/// (case-insensitively); `*` absorbs everything.
fn union_tokens<'a>(values: impl Iterator<Item = &'a str>) -> String {
    let mut tokens: Vec<&str> = Vec::new();
    for value in values {
        for token in value.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            if token == "*" {
                return "*".to_string();
            }
            if !tokens.iter().any(|t| t.eq_ignore_ascii_case(token)) {
                tokens.push(token);
            }
        }
    }
    tokens.join(", ")
}

/// A `resp.set_cookie.*` key no entry of `meta` uses.
fn fresh_cookie_key(meta: &[MetaEntry]) -> String {
    (0usize..)
        .map(|n| format!("{META_RESP_COOKIE_PREFIX}{n}"))
        .find(|key| meta.iter().all(|e| &e.key != key))
        .expect("an unbounded range has an unused key")
}

/// Lay `top` over `base`, `top` winning (see the module docs for identity).
/// Returns the keys of the response headers and cookies `top` wrote into
/// `base` — a cookie whose key collided with an unrelated cookie of `base`
/// is written under a fresh `resp.set_cookie.*` key.
pub(super) fn overlay(base: &mut Vec<MetaEntry>, top: Vec<MetaEntry>) -> Vec<String> {
    // Cookies first, as a set: a producer may legitimately emit two cookies
    // of one name (different attributes), so only `base`'s are replaced.
    let replaced: Vec<CookieId> = top
        .iter()
        .filter(|e| matches!(kind(e), Kind::Cookie))
        .map(|e| cookie_id(&e.value))
        .collect();
    base.retain(|e| !matches!(kind(e), Kind::Cookie) || !replaced.contains(&cookie_id(&e.value)));

    let mut written = Vec::new();
    for entry in top {
        match kind(&entry) {
            Kind::Cookie => {
                let key = if base.iter().any(|e| e.key == entry.key) {
                    fresh_cookie_key(base)
                } else {
                    entry.key
                };
                written.push(key.clone());
                base.push(MetaEntry {
                    key,
                    value: entry.value,
                });
            }
            Kind::Header { name } => {
                let value = if is_listed(UNION_HEADERS, &name) {
                    union_tokens(
                        base.iter()
                            .filter(|e| header_name_is(e, &name))
                            .map(|e| e.value.as_str())
                            .chain(std::iter::once(entry.value.as_str())),
                    )
                } else {
                    entry.value
                };
                let at = base.iter().position(|e| header_name_is(e, &name));
                base.retain(|e| !header_name_is(e, &name));
                let replacement = MetaEntry {
                    key: entry.key,
                    value,
                };
                written.push(replacement.key.clone());
                match at {
                    Some(at) => base.insert(at.min(base.len()), replacement),
                    None => base.push(replacement),
                }
            }
            Kind::Other => match base.iter_mut().find(|e| e.key == entry.key) {
                Some(existing) => existing.value = entry.value,
                None => base.push(entry),
            },
        }
    }
    written
}

/// The entries of `flow_meta` a short-circuit terminal inherits: its response
/// headers and cookies, except a body-describing header ([`BODY_HEADERS`])
/// and anything a responding step wrote (`responder_written`, kept by the
/// executor).
pub(super) fn carried(
    flow_meta: &[MetaEntry],
    responder_written: &HashSet<String>,
) -> Vec<MetaEntry> {
    flow_meta
        .iter()
        .filter(|e| !responder_written.contains(&e.key))
        .filter(|e| match kind(e) {
            Kind::Cookie => true,
            Kind::Header { name } => !is_listed(BODY_HEADERS, &name),
            Kind::Other => false,
        })
        .cloned()
        .collect()
}

/// After a middleware step returned `next`, keep in `responder_written` only
/// the keys whose entry the step passed through unchanged: one it rewrote or
/// removed is the middleware's decision now.
pub(super) fn after_continue(
    responder_written: &mut HashSet<String>,
    before: &[MetaEntry],
    next: &[MetaEntry],
) {
    let value =
        |meta: &[MetaEntry], key: &str| meta.iter().find(|e| e.key == key).map(|e| e.value.clone());
    responder_written.retain(|key| {
        let old = value(before, key);
        old.is_some() && old == value(next, key)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(key: &str, value: &str) -> MetaEntry {
        MetaEntry {
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn a_positional_cookie_key_does_not_clobber_an_unrelated_cookie() {
        let mut base = vec![e("resp.set_cookie.0", "sid=a; Path=/")];
        let written = overlay(&mut base, vec![e("resp.set_cookie.0", "theme=dark")]);
        assert_eq!(
            base,
            vec![
                e("resp.set_cookie.0", "sid=a; Path=/"),
                e("resp.set_cookie.1", "theme=dark"),
            ]
        );
        assert_eq!(written, vec!["resp.set_cookie.1".to_string()]);
    }

    #[test]
    fn a_cookie_of_the_same_name_path_and_domain_is_replaced_whatever_its_key() {
        let mut base = vec![
            e("resp.set_cookie.sid", "sid=old; Path=/"),
            e("resp.set_cookie.other", "sid=api; Path=/api"),
        ];
        overlay(
            &mut base,
            vec![e("resp.set_cookie.0", "sid=new; path=/; HttpOnly")],
        );
        assert_eq!(
            base,
            vec![
                e("resp.set_cookie.other", "sid=api; Path=/api"),
                e("resp.set_cookie.0", "sid=new; path=/; HttpOnly"),
            ]
        );
    }

    #[test]
    fn a_header_is_replaced_case_insensitively_in_place() {
        let mut base = vec![
            e("resp.header.X-Frame-Options", "DENY"),
            e("resp.header.X-Other", "1"),
        ];
        overlay(
            &mut base,
            vec![e("resp.header.x-frame-options", "SAMEORIGIN")],
        );
        assert_eq!(
            base,
            vec![
                e("resp.header.x-frame-options", "SAMEORIGIN"),
                e("resp.header.X-Other", "1"),
            ]
        );
    }

    #[test]
    fn vary_is_unioned() {
        let mut base = vec![e("resp.header.Vary", "Origin")];
        overlay(
            &mut base,
            vec![e("resp.header.vary", "Accept-Encoding, origin")],
        );
        assert_eq!(base, vec![e("resp.header.vary", "Origin, Accept-Encoding")]);

        let mut base = vec![e("resp.header.Vary", "Origin")];
        overlay(&mut base, vec![e("resp.header.Vary", "*")]);
        assert_eq!(base, vec![e("resp.header.Vary", "*")]);
    }

    #[test]
    fn carried_skips_body_headers_status_and_responder_entries() {
        let flow_meta = vec![
            e(
                "resp.header.Access-Control-Allow-Origin",
                "https://a.example",
            ),
            e("resp.header.Cache-Control", "no-store"),
            e("resp.header.ETag", "\"v1\""),
            e("resp.header.Location", "/next"),
            e("resp.header.content-type", "text/html"),
            e("resp.status", "200"),
            e("resp.content_type", "text/html"),
            e("resp.set_cookie.0", "sid=a"),
            e("resp.set_cookie.1", "theme=dark"),
            e("http.header.cookie", "sid=a"),
        ];
        let responder: HashSet<String> = ["resp.set_cookie.1".to_string()].into();
        assert_eq!(
            carried(&flow_meta, &responder),
            vec![
                e(
                    "resp.header.Access-Control-Allow-Origin",
                    "https://a.example"
                ),
                e("resp.header.Cache-Control", "no-store"),
                e("resp.set_cookie.0", "sid=a"),
            ]
        );
    }

    #[test]
    fn a_middleware_rewrite_or_removal_releases_a_responder_key() {
        let before = vec![
            e("resp.header.A", "1"),
            e("resp.header.B", "1"),
            e("resp.header.C", "1"),
        ];
        let next = vec![e("resp.header.A", "1"), e("resp.header.B", "2")];
        let mut set: HashSet<String> = ["resp.header.A", "resp.header.B", "resp.header.C"]
            .map(String::from)
            .into();
        after_continue(&mut set, &before, &next);
        assert_eq!(set, ["resp.header.A".to_string()].into());
    }
}
