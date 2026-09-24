//! How response meta combines inside a flow: a responding step's meta laid
//! over the flow message ([`overlay`]), and the middleware headers a
//! short-circuit terminal inherits ([`carried`]) — where a responding step
//! overwrote a middleware's entry, the displaced middleware entry recorded
//! by [`apply_response`]. See the executor's module docs for where each
//! applies.
//!
//! Identity rules, shared by both:
//! - a header is identified by its name, case-insensitively — except the
//!   list-valued [`UNION_HEADERS`], whose values are unioned (so a `Vary:
//!   Origin` a CORS middleware set survives a terminal's own `Vary`);
//! - a cookie is identified by its name plus its `Path` and `Domain`
//!   attributes ([`CookieId`]), not by its `resp.set_cookie.*` key.
//!   [`wafer_block::response::ResponseBuilder`] and
//!   [`wafer_block::response::cookie_meta`] derive the key from that
//!   identity ([`wafer_block::http_codec::cookie_meta_key`]), but a producer
//!   that writes meta by hand picks any key (`resp.set_cookie.0`), so two
//!   producers' keys can still collide for unrelated cookies;
//! - any other key is identified by the key itself (a content type too,
//!   under either of its keys: the HTTP codec renders the later one).

use std::collections::HashMap;

use wafer_block::{
    core_types::MetaEntry,
    http_codec::{classify_response_meta, cookie_id, CookieId, ResponseMetaPart},
    meta::META_RESP_COOKIE_PREFIX,
};

/// Headers that describe a body (or a redirect to one), never carried from
/// the flow message onto a short-circuit terminal: the terminal has its own
/// body. A content type (any case, either key) and `Content-Length` are not
/// listed because they never classify as a header — the codec reads the
/// first as its content type and refuses the second — so [`carried`] drops
/// them as it drops every non-header entry.
const BODY_HEADERS: &[&str] = &[
    "content-encoding",
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

/// What a meta entry is, for overlay purposes.
enum Kind {
    Cookie,
    Header { name: String },
    Other,
}

fn kind(entry: &MetaEntry) -> Kind {
    // An entry the codec refuses never reaches the wire; it merges by key.
    match classify_response_meta(entry) {
        Ok(Some(ResponseMetaPart::SetCookie(_))) => Kind::Cookie,
        Ok(Some(ResponseMetaPart::Header { name, .. })) => Kind::Header {
            name: name.to_ascii_lowercase(),
        },
        Ok(Some(ResponseMetaPart::Status(_) | ResponseMetaPart::ContentType(_)) | None)
        | Err(_) => Kind::Other,
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

/// One response header or cookie [`overlay`] wrote into `base`.
pub(super) struct Written {
    /// The key it was written under — a cookie whose key collided with an
    /// unrelated cookie of `base` is written under a fresh
    /// `resp.set_cookie.*` key.
    pub(super) key: String,
    /// The `base` entries it displaced: the header of the same name (for a
    /// [`UNION_HEADERS`] header, whose values it absorbed) or the cookies of
    /// the same identity.
    pub(super) displaced: Vec<MetaEntry>,
}

/// Lay `top` over `base`, `top` winning (see the module docs for identity).
/// Returns the response headers and cookies `top` wrote, with what each
/// displaced.
pub(super) fn overlay(base: &mut Vec<MetaEntry>, top: Vec<MetaEntry>) -> Vec<Written> {
    // Cookies first, as a set: a producer may legitimately emit two cookies
    // of one identity, so only `base`'s are displaced — each by the first
    // `top` cookie of that identity.
    let mut displaced_cookies: Vec<(CookieId, Vec<MetaEntry>)> = Vec::new();
    for entry in top.iter().filter(|e| matches!(kind(e), Kind::Cookie)) {
        let id = cookie_id(&entry.value);
        if displaced_cookies.iter().all(|(seen, _)| *seen != id) {
            let displaced = base
                .iter()
                .filter(|e| matches!(kind(e), Kind::Cookie) && cookie_id(&e.value) == id)
                .cloned()
                .collect();
            displaced_cookies.push((id, displaced));
        }
    }
    base.retain(|e| {
        !matches!(kind(e), Kind::Cookie)
            || displaced_cookies
                .iter()
                .all(|(id, _)| *id != cookie_id(&e.value))
    });

    let mut written = Vec::new();
    for entry in top {
        match kind(&entry) {
            Kind::Cookie => {
                let id = cookie_id(&entry.value);
                let displaced = displaced_cookies
                    .iter_mut()
                    .find(|(seen, _)| *seen == id)
                    .map(|(_, displaced)| std::mem::take(displaced))
                    .unwrap_or_default();
                let key = if base.iter().any(|e| e.key == entry.key) {
                    fresh_cookie_key(base)
                } else {
                    entry.key
                };
                written.push(Written {
                    key: key.clone(),
                    displaced,
                });
                base.push(MetaEntry {
                    key,
                    value: entry.value,
                });
            }
            Kind::Header { name } => {
                let displaced: Vec<MetaEntry> = base
                    .iter()
                    .filter(|e| header_name_is(e, &name))
                    .cloned()
                    .collect();
                let value = if is_listed(UNION_HEADERS, &name) {
                    union_tokens(
                        displaced
                            .iter()
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
                written.push(Written {
                    key: replacement.key.clone(),
                    displaced,
                });
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

/// The executor's record of the flow message's response headers and
/// cookies a responding step wrote: each such key, mapped to the
/// middleware entries it displaced (what a short-circuit terminal inherits
/// in its place).
pub(crate) type ResponderRecord = HashMap<String, Vec<MetaEntry>>;

/// Lay a responding step's meta (`top`) over the flow message `msg_meta`
/// and record what it wrote in `record`. What an entry displaced is
/// resolved to middleware entries: a displaced entry an earlier responder
/// wrote contributes the middleware entries IT displaced.
pub(super) fn apply_response(
    record: &mut ResponderRecord,
    msg_meta: &mut Vec<MetaEntry>,
    top: Vec<MetaEntry>,
) {
    for written in overlay(msg_meta, top) {
        let middleware: Vec<MetaEntry> = written
            .displaced
            .into_iter()
            .flat_map(|d| record.get(&d.key).cloned().unwrap_or_else(|| vec![d]))
            .collect();
        record.insert(written.key, middleware);
    }
}

/// The entries of `flow_meta` a short-circuit terminal inherits: its response
/// headers and cookies as the middleware left them — an entry a responding
/// step wrote is replaced by the middleware entries it displaced (`record`)
/// — except a body-describing header ([`BODY_HEADERS`]).
pub(super) fn carried(flow_meta: &[MetaEntry], record: &ResponderRecord) -> Vec<MetaEntry> {
    flow_meta
        .iter()
        .flat_map(|e| match record.get(&e.key) {
            Some(middleware) => middleware.clone(),
            None => vec![e.clone()],
        })
        .filter(|e| match kind(e) {
            Kind::Cookie => true,
            Kind::Header { name } => !is_listed(BODY_HEADERS, &name),
            Kind::Other => false,
        })
        .collect()
}

/// After a middleware step returned `next`, keep in `record` only the keys
/// whose entry the step passed through unchanged: one it rewrote or removed
/// is the middleware's decision now.
pub(super) fn after_continue(
    record: &mut ResponderRecord,
    before: &[MetaEntry],
    next: &[MetaEntry],
) {
    let value =
        |meta: &[MetaEntry], key: &str| meta.iter().find(|e| e.key == key).map(|e| e.value.clone());
    record.retain(|key, _| {
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
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].key, "resp.set_cookie.1");
        assert!(written[0].displaced.is_empty());
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
        let record: ResponderRecord = [("resp.set_cookie.1".to_string(), Vec::new())].into();
        assert_eq!(
            carried(&flow_meta, &record),
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
        let mut record: ResponderRecord = ["resp.header.A", "resp.header.B", "resp.header.C"]
            .map(|k| (k.to_string(), Vec::new()))
            .into();
        after_continue(&mut record, &before, &next);
        assert_eq!(
            record.into_keys().collect::<Vec<_>>(),
            vec!["resp.header.A"]
        );
    }

    /// A responder that rewrites a middleware header leaves the middleware's
    /// value to carry: `Vary` reverts to the middleware's tokens,
    /// `X-Frame-Options` to its value, a displaced cookie comes back.
    #[test]
    fn a_displaced_middleware_entry_is_what_is_carried() {
        let mut msg = vec![
            e("resp.header.Vary", "Origin"),
            e("resp.header.X-Frame-Options", "DENY"),
            e("resp.set_cookie.0", "sid=mw; Path=/"),
        ];
        let mut record = ResponderRecord::new();
        apply_response(
            &mut record,
            &mut msg,
            vec![
                e("resp.header.Vary", "Accept-Encoding"),
                e("resp.header.x-frame-options", "SAMEORIGIN"),
                e("resp.set_cookie.0", "sid=resp; Path=/"),
            ],
        );
        // A second responder over the first still resolves to the middleware.
        apply_response(&mut record, &mut msg, vec![e("resp.header.VARY", "Cookie")]);
        assert_eq!(
            carried(&msg, &record),
            vec![
                e("resp.header.Vary", "Origin"),
                e("resp.header.X-Frame-Options", "DENY"),
                e("resp.set_cookie.0", "sid=mw; Path=/"),
            ]
        );
    }

    #[test]
    fn cookie_paths_are_case_sensitive_and_a_missing_path_is_its_own() {
        let mut base = vec![
            e("resp.set_cookie.a", "sid=1; Path=/API"),
            e("resp.set_cookie.b", "sid=2"),
            e("resp.set_cookie.c", "sid=3; Path=/; Domain=.A.example"),
        ];
        overlay(
            &mut base,
            vec![
                e("resp.set_cookie.x", "sid=9; Path=/api"),
                e("resp.set_cookie.y", "sid=8; path=/; domain=a.example"),
            ],
        );
        assert_eq!(
            base.iter().map(|e| e.value.as_str()).collect::<Vec<_>>(),
            vec![
                "sid=1; Path=/API",
                "sid=2",
                "sid=9; Path=/api",
                "sid=8; path=/; domain=a.example"
            ]
        );
    }
}
