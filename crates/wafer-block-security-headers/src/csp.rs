//! Parsing and merging of Content-Security-Policy directive lists.
//!
//! [`merge_csp`] folds operator-supplied directives into a baseline policy so
//! that the result can only admit what the baseline admits plus sources that
//! cannot run another origin's script in this site's origin. Everything the
//! merge refuses is reported back as a [`Refusal`] rather than silently
//! dropped, so the block can log it.

use std::{collections::BTreeMap, fmt};

/// Directives whose sources decide which script runs with this site's
/// origin: the script directives themselves, `worker-src`, `child-src`
/// (`worker-src`'s fallback) and `default-src` (every fetch directive's
/// fallback).
const SCRIPT_DIRECTIVES: &[&str] = &[
    "default-src",
    "script-src",
    "script-src-elem",
    "script-src-attr",
    "worker-src",
    "child-src",
];

/// Directives the operator may narrow but never widen past the baseline:
/// widening either lets injected markup send the document's forms, or
/// resolve its relative URLs, to another origin.
const NARROW_ONLY_DIRECTIVES: &[&str] = &["base-uri", "form-action"];

/// Owned by the block's `frame_ancestors` config key, which also drives
/// `X-Frame-Options`; the operator CSP cannot set it.
const FRAME_ANCESTORS: &str = "frame-ancestors";

/// A directive or source the merge refused, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// The directive as written in the operator policy.
    pub directive: String,
    /// The refused source, or `None` when the whole directive was refused.
    pub source: Option<String>,
    /// Why it was refused.
    pub reason: &'static str,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            Some(source) => write!(f, "`{}` in `{}`: {}", source, self.directive, self.reason),
            None => write!(f, "directive `{}`: {}", self.directive, self.reason),
        }
    }
}

/// The outcome of [`merge_csp`]: the policy to send and everything refused
/// on the way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CspMerge {
    /// The merged policy, one `name sources…` entry per directive, directive
    /// names lower-case and each emitted exactly once.
    pub policy: String,
    /// Operator directives and sources left out of `policy`.
    pub refused: Vec<Refusal>,
}

/// Directive name → sources. Names are lower-cased on parse, so a directive
/// is stored — and later serialized — exactly once whatever its spelling.
type Directives = BTreeMap<String, Vec<String>>;

/// Merge `custom` into `baseline` directive by directive.
///
/// Both inputs are `name source…; name source…` policy strings. Directive
/// names compare ASCII case-insensitively and, as in browsers, only the first
/// occurrence of a name counts; later duplicates are refused. A token that is
/// not a valid directive name or source (a comma, a non-ASCII or control
/// character) is refused, so nothing the operator writes can change how the
/// policy splits into directives or policies.
///
/// Every baseline directive survives. Operator directives then apply as:
/// * `frame-ancestors` — refused; the `frame_ancestors` config key owns it.
/// * `base-uri`, `form-action` — narrow only: a source already in the
///   baseline's list is kept, `'none'` replaces the list, anything else is
///   refused.
/// * `default-src`, `script-src`, `script-src-elem`, `script-src-attr`,
///   `worker-src`, `child-src` — sources are added to the baseline's, except
///   any that would let script run from an arbitrary origin or from a string:
///   `*`, a scheme-only source (`https:`, `data:`, `blob:`, …), a host-source
///   whose host is `*` at any scheme, port or path (`https://*/`, `*:443`), a
///   wildcard over a single label (`*.com`), `'unsafe-eval'`, and any
///   unrecognised keyword or malformed source.
/// * any other directive — sources are added to the baseline's.
///
/// A directive the baseline lacks is added only when at least one of its
/// sources is accepted (or it takes none, like `upgrade-insecure-requests`):
/// an operator directive whose sources were all refused would otherwise
/// become an empty — block-everything — directive.
pub fn merge_csp(baseline: &str, custom: &str) -> CspMerge {
    let mut refused = Vec::new();
    let mut merged = parse(baseline, &mut refused);

    for (name, sources) in parse(custom, &mut refused) {
        if name == FRAME_ANCESTORS {
            refused.push(Refusal {
                directive: name,
                source: None,
                reason: "set by the `frame_ancestors` config key, not the CSP",
            });
            continue;
        }

        if NARROW_ONLY_DIRECTIVES.contains(&name.as_str()) {
            if let Some(base) = merged.get_mut(&name) {
                narrow(&name, base, sources, &mut refused);
                continue;
            }
        }

        let script = SCRIPT_DIRECTIVES.contains(&name.as_str());
        let offered = sources.len();
        let accepted: Vec<String> = sources
            .into_iter()
            .filter(
                |source| match script.then(|| script_source_refusal(source)).flatten() {
                    Some(reason) => {
                        refused.push(Refusal {
                            directive: name.clone(),
                            source: Some(source.clone()),
                            reason,
                        });
                        false
                    }
                    None => true,
                },
            )
            .collect();

        if accepted.is_empty() && offered > 0 && !merged.contains_key(&name) {
            continue;
        }
        let entry = merged.entry(name).or_default();
        for source in accepted {
            add_source(entry, source);
        }
    }

    CspMerge {
        policy: serialize(&merged),
        refused,
    }
}

/// Parse a policy into [`Directives`], refusing invalid names, duplicate
/// names and invalid source tokens.
fn parse(input: &str, refused: &mut Vec<Refusal>) -> Directives {
    let mut out = Directives::new();
    for raw in input.split(';') {
        let mut tokens = raw.split_ascii_whitespace();
        let Some(written) = tokens.next() else {
            continue;
        };
        if !is_directive_name(written) {
            refused.push(Refusal {
                directive: written.to_string(),
                source: None,
                reason: "not a valid directive name",
            });
            continue;
        }
        let name = written.to_ascii_lowercase();
        if out.contains_key(&name) {
            refused.push(Refusal {
                directive: written.to_string(),
                source: None,
                reason: "duplicate directive; browsers use only the first",
            });
            continue;
        }
        let mut sources = Vec::new();
        let mut offered = 0;
        for token in tokens {
            offered += 1;
            if is_value_token(token) {
                sources.push(token.to_string());
            } else {
                refused.push(Refusal {
                    directive: written.to_string(),
                    source: Some(token.to_string()),
                    reason: "not a valid CSP token (comma, control or non-ASCII character)",
                });
            }
        }
        // Every token refused: leave the directive out rather than emit it
        // empty, which would block everything it governs.
        if offered > 0 && sources.is_empty() {
            continue;
        }
        out.insert(name, sources);
    }
    out
}

/// Apply an operator list to a narrow-only baseline directive: keep only
/// sources the baseline already has, or collapse to `'none'`.
fn narrow(name: &str, base: &mut Vec<String>, sources: Vec<String>, refused: &mut Vec<Refusal>) {
    let mut kept = Vec::new();
    for source in sources {
        let known = source.eq_ignore_ascii_case("'none'")
            || base.iter().any(|b| b.eq_ignore_ascii_case(&source));
        if known {
            kept.push(source);
        } else {
            refused.push(Refusal {
                directive: name.to_string(),
                source: Some(source),
                reason: "can only narrow the baseline for this directive",
            });
        }
    }
    if kept.iter().any(|s| s.eq_ignore_ascii_case("'none'")) {
        *base = vec!["'none'".to_string()];
    } else if !kept.is_empty() {
        *base = kept;
    }
}

/// Add `source` to a directive's list. `'none'` only means "nothing" when it
/// is the whole list, so it is dropped from a list with other sources.
fn add_source(list: &mut Vec<String>, source: String) {
    let is_none = |s: &String| s.eq_ignore_ascii_case("'none'");
    if list.iter().any(|s| s.eq_ignore_ascii_case(&source)) {
        return;
    }
    if is_none(&source) {
        if list.is_empty() {
            list.push(source);
        }
        return;
    }
    list.retain(|s| !is_none(s));
    list.push(source);
}

fn serialize(directives: &Directives) -> String {
    directives
        .iter()
        .map(|(name, sources)| {
            if sources.is_empty() {
                name.clone()
            } else {
                format!("{name} {}", sources.join(" "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// CSP `directive-name`: `1*( ALPHA / DIGIT / "-" )`.
fn is_directive_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A CSP directive-value token: visible ASCII other than `,` (which starts
/// another policy) and `;` (which starts another directive).
fn is_value_token(s: &str) -> bool {
    s.bytes()
        .all(|b| (0x21..=0x7e).contains(&b) && b != b',' && b != b';')
}

/// Why a source may not appear in a [script directive](SCRIPT_DIRECTIVES),
/// or `None` when it may.
pub(crate) fn script_source_refusal(source: &str) -> Option<&'static str> {
    let s = source.to_ascii_lowercase();
    if s.starts_with('\'') {
        return keyword_refusal(&s);
    }
    match parse_source(&s) {
        Source::Wildcard => Some("`*` admits script from any origin"),
        Source::Scheme => Some(
            "a scheme-only source admits script from every host on that scheme, or \
             (data:, blob:, filesystem:) from strings the page builds",
        ),
        Source::Host(Host::Any) => Some("a `*` host admits script from any origin"),
        Source::Host(Host::Subdomains { suffix }) if !suffix.contains('.') => {
            Some("a wildcard over a single label admits script from any registrable domain")
        }
        Source::Host(_) => None,
        Source::Invalid => Some("not a valid CSP source expression"),
    }
}

/// Quoted sources allowed in a script directive: keywords that do not let a
/// string become script, nonces and hashes. `s` is lower-case.
fn keyword_refusal(s: &str) -> Option<&'static str> {
    match s {
        "'self'" | "'none'" | "'unsafe-inline'" | "'unsafe-hashes'" | "'strict-dynamic'"
        | "'wasm-unsafe-eval'" | "'report-sample'" => None,
        "'unsafe-eval'" => Some("`'unsafe-eval'` lets any string run as script"),
        _ => {
            let inner = s.strip_prefix('\'').and_then(|s| s.strip_suffix('\''));
            let value = inner.and_then(|inner| {
                ["nonce-", "sha256-", "sha384-", "sha512-"]
                    .iter()
                    .find_map(|prefix| inner.strip_prefix(prefix))
            });
            match value {
                Some(v) if is_base64_value(v) => None,
                _ => Some("not a recognised keyword, nonce or hash source"),
            }
        }
    }
}

/// CSP `base64-value`: `1*( ALPHA / DIGIT / "+" / "/" / "-" / "_" )*2( "=" )`.
fn is_base64_value(v: &str) -> bool {
    let body = v.trim_end_matches('=');
    !body.is_empty()
        && v.len() - body.len() <= 2
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'-' | b'_'))
}

/// An unquoted source expression, classified by what it can match.
#[derive(Debug, PartialEq, Eq)]
enum Source<'a> {
    /// `*`.
    Wildcard,
    /// `scheme:`.
    Scheme,
    /// `[scheme://]host[:port][/path]`.
    Host(Host<'a>),
    /// Anything the CSP grammar does not accept.
    Invalid,
}

#[derive(Debug, PartialEq, Eq)]
enum Host<'a> {
    /// `*`: every host.
    Any,
    /// `*.suffix`: every subdomain of `suffix`.
    Subdomains { suffix: &'a str },
    /// One named host.
    Exact,
}

/// Classify a lower-case unquoted source by the CSP `scheme-source` /
/// `host-source` grammar.
fn parse_source(s: &str) -> Source<'_> {
    if s == "*" {
        return Source::Wildcard;
    }
    if let Some(scheme) = s.strip_suffix(':') {
        return if is_scheme(scheme) {
            Source::Scheme
        } else {
            Source::Invalid
        };
    }
    let rest = match s.split_once("://") {
        Some((scheme, rest)) if is_scheme(scheme) => rest,
        Some(_) => return Source::Invalid,
        None => s,
    };
    let host_end = rest.find([':', '/']).unwrap_or(rest.len());
    let (host, after_host) = rest.split_at(host_end);
    let path = match after_host.strip_prefix(':') {
        Some(port_and_path) => {
            let port_end = port_and_path.find('/').unwrap_or(port_and_path.len());
            let (port, path) = port_and_path.split_at(port_end);
            if port != "*" && (port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit())) {
                return Source::Invalid;
            }
            path
        }
        None => after_host,
    };
    if !(path.is_empty() || path.starts_with('/')) {
        return Source::Invalid;
    }
    if host == "*" {
        return Source::Host(Host::Any);
    }
    match host.strip_prefix("*.") {
        Some(suffix) if is_host_labels(suffix) => Source::Host(Host::Subdomains { suffix }),
        Some(_) => Source::Invalid,
        None if is_host_labels(host) => Source::Host(Host::Exact),
        None => Source::Invalid,
    }
}

/// CSP `scheme`: `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`.
fn is_scheme(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// `1*host-char *( "." 1*host-char )`, `host-char = ALPHA / DIGIT / "-"`.
fn is_host_labels(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; \
                        frame-ancestors 'none'; base-uri 'self'; form-action 'self'";

    /// The sources of `name` in a serialized policy, or `None` when absent.
    fn directive<'a>(policy: &'a str, name: &str) -> Option<Vec<&'a str>> {
        policy.split(';').find_map(|d| {
            let mut tokens = d.split_ascii_whitespace();
            (tokens.next() == Some(name)).then(|| tokens.collect())
        })
    }

    #[test]
    fn script_sources_that_admit_any_origin_or_a_string_are_refused() {
        for source in [
            "*",
            "https:",
            "HTTPS:",
            "http:",
            "ws:",
            "data:",
            "blob:",
            "filesystem:",
            "https://*",
            "https://*/",
            "https://*:443",
            "https://*:*/x",
            "*:443",
            "*/path",
            "*.com",
            "https://*.com",
            "'unsafe-eval'",
            "'UNSAFE-EVAL'",
            "'unknown-keyword'",
            "'nonce-'",
            "https://cdn.exa_mple.com",
            "https://cdn.example.com:44x",
            "1http://cdn.example.com",
        ] {
            assert!(
                script_source_refusal(source).is_some(),
                "{source} should be refused"
            );
        }
        for source in [
            "'self'",
            "'SELF'",
            "'none'",
            "'unsafe-inline'",
            "'strict-dynamic'",
            "'wasm-unsafe-eval'",
            "'nonce-abc123=='",
            "'sha256-xyz+/_-'",
            "https://cdn.example.com",
            "https://cdn.example.com:*",
            "https://cdn.example.com:8443/js/",
            "cdn.example.com",
            "*.example.com",
            "https://*.example.com",
        ] {
            assert_eq!(
                script_source_refusal(source),
                None,
                "{source} should be accepted"
            );
        }
    }

    #[test]
    fn directive_names_merge_case_insensitively_and_appear_once() {
        let merged = merge_csp(BASE, "SCRIPT-SRC https://cdn.example.com; Default-Src *");
        let names: Vec<&str> = merged
            .policy
            .split(';')
            .filter_map(|d| d.split_ascii_whitespace().next())
            .collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(names.len(), unique.len(), "{}", merged.policy);
        assert!(names.iter().all(|n| *n == n.to_ascii_lowercase()));
        assert_eq!(
            directive(&merged.policy, "script-src"),
            Some(vec!["'self'", "'unsafe-inline'", "https://cdn.example.com"])
        );
        assert_eq!(
            directive(&merged.policy, "default-src"),
            Some(vec!["'self'"])
        );
    }

    #[test]
    fn a_later_duplicate_directive_is_refused_as_browsers_ignore_it() {
        let merged = merge_csp(BASE, "img-src https://a.example; IMG-SRC https://b.example");
        assert_eq!(
            directive(&merged.policy, "img-src"),
            Some(vec!["https://a.example"])
        );
        assert_eq!(merged.refused.len(), 1, "{:?}", merged.refused);
    }

    #[test]
    fn frame_ancestors_is_refused_in_any_case() {
        for custom in [
            "frame-ancestors *",
            "FRAME-ANCESTORS *",
            "Frame-Ancestors 'self'",
        ] {
            let merged = merge_csp(BASE, custom);
            assert_eq!(
                directive(&merged.policy, "frame-ancestors"),
                Some(vec!["'none'"]),
                "{custom}: {}",
                merged.policy
            );
            assert!(
                !merged
                    .policy
                    .to_ascii_lowercase()
                    .contains("frame-ancestors *"),
                "{}",
                merged.policy
            );
        }
    }

    #[test]
    fn base_uri_and_form_action_only_narrow() {
        let merged = merge_csp(BASE, "form-action https: 'self'; base-uri 'none'");
        assert_eq!(
            directive(&merged.policy, "form-action"),
            Some(vec!["'self'"])
        );
        assert_eq!(directive(&merged.policy, "base-uri"), Some(vec!["'none'"]));
        assert_eq!(
            merged.refused,
            vec![Refusal {
                directive: "form-action".into(),
                source: Some("https:".into()),
                reason: "can only narrow the baseline for this directive",
            }]
        );
    }

    #[test]
    fn a_new_script_directive_whose_sources_are_all_refused_is_not_added() {
        // An empty `script-src-elem` would block every script element.
        let merged = merge_csp(BASE, "script-src-elem https: *");
        assert_eq!(directive(&merged.policy, "script-src-elem"), None);
        assert_eq!(merged.refused.len(), 2);
    }

    #[test]
    fn source_less_directives_are_kept() {
        let merged = merge_csp(BASE, "upgrade-insecure-requests");
        assert_eq!(
            directive(&merged.policy, "upgrade-insecure-requests"),
            Some(vec![])
        );
    }

    #[test]
    fn tokens_that_would_split_the_policy_are_refused() {
        // A comma starts a second policy; U+00A0 is not CSP whitespace, so
        // it would glue two tokens into one.
        let merged = merge_csp(
            BASE,
            "img-src https://a.example,script-src; font-src a\u{a0}b",
        );
        assert!(!merged.policy.contains(','), "{}", merged.policy);
        assert!(!merged.policy.contains('\u{a0}'), "{}", merged.policy);
        assert_eq!(merged.refused.len(), 2, "{:?}", merged.refused);
    }

    #[test]
    fn none_is_dropped_once_a_directive_has_other_sources() {
        let merged = merge_csp(BASE, "media-src 'none'; img-src 'none' https://a.example");
        assert_eq!(directive(&merged.policy, "media-src"), Some(vec!["'none'"]));
        assert_eq!(
            directive(&merged.policy, "img-src"),
            Some(vec!["https://a.example"])
        );
    }
}
