//! Security-headers block — appends a baseline of HTTP response headers
//! (Content-Security-Policy, Strict-Transport-Security, X-Content-Type-Options,
//! X-Frame-Options, Referrer-Policy, Permissions-Policy) to every message
//! that passes through it. The CSP is tenant-configurable via the `csp`
//! [`ConfigVar`] but the operator-supplied directives are merged on top of
//! a restrictive baseline (see [`merge_csp`]) so they can only widen the
//! policy in safe ways.

#![warn(missing_docs)]

use std::sync::OnceLock;

use wafer_block::*;

/// Baseline CSP that the block always enforces, regardless of `cfg.csp`.
///
/// `cfg.csp` directives are merged *on top of* this baseline rather than
/// replacing it — tenants can extend (add hashes/origins/etc.) but cannot
/// weaken `default-src` to `*` or re-enable `unsafe-eval`.
const DEFAULT_CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self' https:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'";

/// Who may frame this site's documents. Drives both `frame-ancestors` in
/// the CSP and the legacy `X-Frame-Options` header, so the two can never
/// disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FrameAncestors {
    None,
    SelfOrigin,
}

impl FrameAncestors {
    fn csp_source(self) -> &'static str {
        match self {
            Self::None => "'none'",
            Self::SelfOrigin => "'self'",
        }
    }
    fn x_frame_options(self) -> &'static str {
        match self {
            Self::None => "DENY",
            Self::SelfOrigin => "SAMEORIGIN",
        }
    }
}

/// SecurityHeadersBlock adds standard security headers to responses.
///
/// CSP is configurable via `block_config` — the runtime serializes the
/// config JSON to bytes and passes them in at `lifecycle(Init)`. Until
/// Init sets a value, the block uses the restrictive [`DEFAULT_CSP`]. Stored
/// via `OnceLock<String>` because `handle` takes `&self` and the config is
/// written once at Init, then read on every request.
///
/// The CSP applied at request time is `merge_csp(DEFAULT_CSP, cfg.csp)`,
/// which guarantees:
/// * `default-src` never widens past the baseline (no `*`),
/// * `script-src` never gains `'unsafe-eval'`,
///
/// regardless of what the operator puts in `cfg.csp`. See `merge_csp`.
pub struct SecurityHeadersBlock {
    csp: OnceLock<String>,
    /// Init-resolved `frame_ancestors` policy. Unset until Init parses the
    /// `frame_ancestors` config key; `effective_frame_ancestors` falls back
    /// to `FrameAncestors::None` (today's default) until then.
    frame_ancestors: OnceLock<FrameAncestors>,
}

impl Default for SecurityHeadersBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl SecurityHeadersBlock {
    /// Build a new block. The effective CSP defaults to the restrictive
    /// [`DEFAULT_CSP`] until `lifecycle(Init)` sets a merged operator policy.
    ///
    /// Any operator-supplied `csp` config replaces the default (after merging
    /// through [`merge_csp`]) the first time the runtime fires the `Init`
    /// lifecycle event.
    pub fn new() -> Self {
        Self {
            csp: OnceLock::new(),
            frame_ancestors: OnceLock::new(),
        }
    }

    /// The CSP applied to responses: the Init-set merged value, or
    /// [`DEFAULT_CSP`] when Init has not (yet) supplied one.
    fn effective_csp(&self) -> &str {
        self.csp.get().map_or(DEFAULT_CSP, String::as_str)
    }

    /// The frame-ancestors policy applied to responses: the Init-set value,
    /// or [`FrameAncestors::None`] (today's restrictive default) when Init
    /// has not (yet) supplied one.
    fn effective_frame_ancestors(&self) -> FrameAncestors {
        self.frame_ancestors
            .get()
            .copied()
            .unwrap_or(FrameAncestors::None)
    }
}

#[wafer_async_trait]
impl Block for SecurityHeadersBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/security-headers",
            "0.0.1",
            "middleware@v1",
            "Adds standard security headers to HTTP responses",
        )
        .infrastructure()
        .flow_config(vec![
            ConfigVar::new(
                "csp",
                "Operator-supplied Content-Security-Policy directives, merged \
                 on top of the block's restrictive baseline (see merge_csp).",
                "",
            )
            .name("CSP"),
            ConfigVar::new(
                "frame_ancestors",
                "`none` (default: frame-ancestors 'none' + X-Frame-Options DENY) or `self` \
                 (same-origin framing allowed: frame-ancestors 'self' + SAMEORIGIN).",
                "none",
            )
            .name("Frame ancestors"),
        ])
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let frame_ancestors = self.effective_frame_ancestors();
        let csp = with_frame_ancestors(self.effective_csp(), frame_ancestors);

        let mut out_msg = msg;
        out_msg.set_meta("resp.header.X-Content-Type-Options", "nosniff");
        out_msg.set_meta(
            "resp.header.X-Frame-Options",
            frame_ancestors.x_frame_options(),
        );
        // SEC-085: X-XSS-Protection is deprecated. The legacy IE filter it
        // toggled was removed from modern browsers and can introduce XSS
        // in some configurations; CSP is the modern replacement. Header
        // intentionally omitted.
        out_msg.set_meta(
            "resp.header.Referrer-Policy",
            "strict-origin-when-cross-origin",
        );
        out_msg.set_meta("resp.header.Content-Security-Policy", csp);
        // SEC-086: include `preload` so the policy is eligible for the
        // HSTS preload list (https://hstspreload.org). Submission to the
        // list is a separate manual step; emitting the directive is the
        // prerequisite.
        out_msg.set_meta(
            "resp.header.Strict-Transport-Security",
            "max-age=31536000; includeSubDomains; preload",
        );
        out_msg.set_meta(
            "resp.header.Permissions-Policy",
            "camera=(), microphone=(), geolocation=()",
        );

        OutputStream::continue_with(out_msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            let config = BlockConfig::from_event(&event);
            if let Some(custom_csp) = config.get("csp").and_then(|v| v.as_str()) {
                let merged = merge_csp(DEFAULT_CSP, custom_csp);
                // Write-once: Init fires a single time per registration.
                let _ = self.csp.set(merged);
            }
            match config.str_or("frame_ancestors", "none") {
                "self" => {
                    let _ = self.frame_ancestors.set(FrameAncestors::SelfOrigin);
                }
                _ => {
                    let _ = self.frame_ancestors.set(FrameAncestors::None);
                }
            }
        }
        Ok(())
    }
}

/// SEC-08: whether a CSP source is "broad" — one that would widen a
/// script/default policy to essentially any origin. Rejected for `script-src`
/// and `default-src`:
/// - the literal wildcard `*`,
/// - a scheme-only source (`https:`, `http:`, `data:`, `blob:`, …) — matches
///   every origin on that scheme,
/// - a bare-wildcard host (`https://*`).
///
/// Specific host sources (`https://cdn.example.com`), subdomain wildcards
/// (`https://*.example.com`, `*.example.com`), nonces (`'nonce-…'`), hashes
/// (`'sha256-…'`) and keywords (`'self'`, `'unsafe-inline'`) are NOT broad and
/// pass through.
pub(crate) fn is_broad_source(src: &str) -> bool {
    let s = src.trim();
    if s == "*" {
        return true;
    }
    if let Some(idx) = s.find(':') {
        let scheme = &s[..idx];
        let rest = &s[idx + 1..];
        let scheme_ok = !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if scheme_ok {
            // "https:" (scheme-only) or "https://*" (bare-wildcard host).
            let host = rest.trim_start_matches("//");
            if rest.is_empty() || host == "*" {
                return true;
            }
        }
    }
    false
}

/// Rewrite `csp`'s `frame-ancestors` directive to `fa`'s source, leaving
/// every other directive untouched. `frame_ancestors` is the only knob that
/// can change this directive — the operator `csp` config key cannot (see
/// `merge_csp`).
fn with_frame_ancestors(csp: &str, fa: FrameAncestors) -> String {
    csp.split(';')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| {
            if d.starts_with("frame-ancestors") {
                format!("frame-ancestors {}", fa.csp_source())
            } else {
                d.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Merge `custom` into `baseline` directive-by-directive.
///
/// Both inputs are standard `directive value...; directive value...` CSP
/// strings. The result preserves every baseline directive (so the operator
/// cannot remove `frame-ancestors 'none'` or similar) and then applies the
/// custom values per directive subject to **non-weakening rules**:
///
/// * `default-src` and `script-src` — [broad sources](is_broad_source)
///   (`*`, scheme-only, bare-wildcard host) are dropped; `default-src`
///   always re-adds `'self'` if missing.
/// * `script-src` — `'unsafe-eval'` is always stripped.
/// * Any directive present only in `custom` is appended verbatim.
///
/// All other directives merge as the union of (baseline ∪ custom) sources
/// with duplicates removed and ordering preserved.
pub fn merge_csp(baseline: &str, custom: &str) -> String {
    use std::collections::BTreeMap;

    fn parse(input: &str) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for raw in input.split(';') {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mut parts = trimmed.split_whitespace();
            let directive = match parts.next() {
                Some(d) => d.to_string(),
                None => continue,
            };
            let sources: Vec<String> = parts.map(|s| s.to_string()).collect();
            out.entry(directive).or_default().extend(sources);
        }
        out
    }

    let base = parse(baseline);
    let extra = parse(custom);

    // Start from the baseline and merge each custom directive.
    let mut merged: BTreeMap<String, Vec<String>> = base;

    for (directive, custom_sources) in extra {
        let entry = merged.entry(directive.clone()).or_default();
        for src in custom_sources {
            // Non-weakening rules.
            let is_default_src = directive.eq_ignore_ascii_case("default-src");
            let is_script_src = directive.eq_ignore_ascii_case("script-src");
            let lower = src.to_lowercase();
            // SEC-08: reject broad sources (wildcard, scheme-only, bare-wildcard
            // host) that would widen script/default policy to any origin.
            // Specific hosts, subdomain wildcards, nonces and hashes still pass.
            if (is_default_src || is_script_src) && is_broad_source(&lower) {
                continue;
            }
            if is_script_src && lower == "'unsafe-eval'" {
                continue;
            }
            if !entry.iter().any(|s| s == &src) {
                entry.push(src);
            }
        }
    }

    // Final pass: ensure baseline guarantees survive even if the operator
    // tried to clear a directive entirely.
    if let Some(sources) = merged.get_mut("default-src") {
        if !sources.iter().any(|s| s == "'self'") {
            sources.insert(0, "'self'".to_string());
        }
        sources.retain(|s| !is_broad_source(s));
    }
    if let Some(sources) = merged.get_mut("script-src") {
        sources.retain(|s| s.to_lowercase() != "'unsafe-eval'" && !is_broad_source(s));
    }

    // Re-serialize in a stable directive order (BTreeMap iterates sorted).
    merged
        .into_iter()
        .map(|(d, srcs)| {
            if srcs.is_empty() {
                d
            } else {
                format!("{d} {}", srcs.join(" "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

wafer_block::register_static_block!("wafer-run/security-headers", SecurityHeadersBlock);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_csp_preserves_baseline_when_custom_empty() {
        let merged = merge_csp(DEFAULT_CSP, "");
        // Every baseline directive must still appear.
        assert!(merged.contains("default-src 'self'"));
        assert!(merged.contains("frame-ancestors 'none'"));
        assert!(merged.contains("base-uri 'self'"));
    }

    #[test]
    fn merge_csp_strips_unsafe_eval_from_custom_script_src() {
        let merged = merge_csp(
            DEFAULT_CSP,
            "script-src 'self' 'unsafe-eval' https://cdn.example.com",
        );
        // unsafe-eval must be stripped, cdn must be added, baseline values kept.
        assert!(!merged.contains("'unsafe-eval'"));
        assert!(merged.contains("https://cdn.example.com"));
        assert!(merged.contains("'unsafe-inline'")); // from baseline
    }

    // SEC-08: scheme-only and bare-wildcard-host sources are "broad" and must
    // be rejected from script/default policy; specific hosts, subdomain
    // wildcards, nonces and hashes are not.
    #[test]
    fn is_broad_source_classification() {
        for broad in ["*", "https:", "http:", "data:", "blob:", "ws:", "https://*"] {
            assert!(is_broad_source(broad), "{broad} should be broad");
        }
        for ok in [
            "'self'",
            "'unsafe-inline'",
            "'nonce-abc123'",
            "'sha256-xyz'",
            "https://cdn.example.com",
            "*.example.com",
            "https://*.example.com",
        ] {
            assert!(!is_broad_source(ok), "{ok} should NOT be broad");
        }
    }

    // SEC-08: `script-src https:` (etc.) previously passed, authorizing scripts
    // from every origin on that scheme. Broad sources are now stripped while
    // specific hosts and nonces survive.
    #[test]
    fn merge_csp_strips_broad_script_sources_keeps_specific_host() {
        let merged = merge_csp(
            DEFAULT_CSP,
            "script-src https: data: https://cdn.example.com 'nonce-abc'",
        );
        let script = merged
            .split(';')
            .map(|d| d.trim())
            .find(|d| d.starts_with("script-src"))
            .expect("script-src present");
        let sources: Vec<&str> = script.split_whitespace().skip(1).collect();
        assert!(
            !sources.contains(&"https:"),
            "scheme-only https: stripped: {script}"
        );
        assert!(!sources.contains(&"data:"), "data: stripped: {script}");
        assert!(
            sources.contains(&"https://cdn.example.com"),
            "specific host kept: {script}"
        );
        assert!(sources.contains(&"'nonce-abc'"), "nonce kept: {script}");
    }

    #[test]
    fn merge_csp_rejects_wildcard_default_src() {
        let merged = merge_csp(DEFAULT_CSP, "default-src *");
        assert!(merged.contains("default-src 'self'"));
        // `*` must not appear as a default-src source.
        let default_section = merged
            .split(';')
            .find(|s| s.trim().starts_with("default-src"))
            .unwrap_or("");
        assert!(!default_section.split_whitespace().any(|t| t == "*"));
    }

    #[test]
    fn merge_csp_allows_extension_with_new_directive() {
        let merged = merge_csp(DEFAULT_CSP, "worker-src 'self' blob:");
        assert!(merged.contains("worker-src 'self' blob:"));
        // Baseline still intact.
        assert!(merged.contains("frame-ancestors 'none'"));
    }

    // --- frame_ancestors -----------------------------------------------

    fn init_event(json: &str) -> LifecycleEvent {
        LifecycleEvent {
            event_type: LifecycleType::Init,
            data: json.as_bytes().to_vec(),
        }
    }

    /// Minimal Context shim — SecurityHeadersBlock never reads `ctx` in
    /// `handle`/`lifecycle`, so every method is a stub.
    #[derive(Clone)]
    struct NoopCtx;

    #[wafer_async_trait]
    impl Context for NoopCtx {
        async fn call_block(
            &self,
            _block_name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::respond(b"unused".to_vec())
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }
        fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    /// Test-only accessor for reading the `Message` out of an `OutputStream`
    /// that terminated with `Continue` — the shape a middleware's `handle`
    /// returns. Not part of `OutputStream`'s public API; local to this
    /// crate's tests only.
    trait OutputStreamTestExt {
        async fn into_continue_message(self) -> Option<Message>;
    }

    impl OutputStreamTestExt for OutputStream {
        async fn into_continue_message(self) -> Option<Message> {
            match self.collect_buffered().await {
                Err(TerminalNotResponse::Continue(msg)) => Some(msg),
                _ => None,
            }
        }
    }

    #[tokio::test]
    async fn frame_ancestors_self_relaxes_both_headers() {
        let block = SecurityHeadersBlock::new();
        block
            .lifecycle(&NoopCtx, init_event(r#"{"frame_ancestors":"self"}"#))
            .await
            .unwrap();
        let out = block
            .handle(&NoopCtx, Message::new("retrieve:/"), InputStream::empty())
            .await;
        let msg = out
            .into_continue_message()
            .await
            .expect("middleware continues");
        assert_eq!(msg.get_meta("resp.header.X-Frame-Options"), "SAMEORIGIN");
        let csp = msg.get_meta("resp.header.Content-Security-Policy");
        assert!(csp.contains("frame-ancestors 'self'"), "{csp}");
        assert!(!csp.contains("frame-ancestors 'none'"), "{csp}");
    }

    #[tokio::test]
    async fn frame_ancestors_defaults_to_none_and_deny() {
        let block = SecurityHeadersBlock::new();
        block
            .lifecycle(&NoopCtx, init_event(r#"{}"#))
            .await
            .unwrap();
        let out = block
            .handle(&NoopCtx, Message::new("retrieve:/"), InputStream::empty())
            .await;
        let msg = out
            .into_continue_message()
            .await
            .expect("middleware continues");
        assert_eq!(msg.get_meta("resp.header.X-Frame-Options"), "DENY");
        assert!(msg
            .get_meta("resp.header.Content-Security-Policy")
            .contains("frame-ancestors 'none'"));
    }

    #[test]
    fn merge_csp_cannot_relax_frame_ancestors_through_the_csp_key() {
        // The knob is `frame_ancestors`, never the operator CSP string.
        let merged = merge_csp(DEFAULT_CSP, "frame-ancestors 'self'");
        assert!(merged.contains("frame-ancestors 'none'"), "{merged}");
    }
}
