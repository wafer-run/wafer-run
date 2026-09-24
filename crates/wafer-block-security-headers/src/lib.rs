//! Security-headers block — appends a baseline of HTTP response headers
//! (Content-Security-Policy, Strict-Transport-Security, X-Content-Type-Options,
//! X-Frame-Options, Referrer-Policy, Permissions-Policy) to every message
//! that passes through it. The CSP is tenant-configurable via the `csp`
//! [`ConfigVar`] but the operator-supplied directives are merged on top of
//! a restrictive baseline (see [`merge_csp`]) so they can only widen the
//! policy in safe ways.

#![warn(missing_docs)]

mod csp;

use std::sync::OnceLock;

pub use csp::{merge_csp, CspMerge, Refusal};
use wafer_block::*;

/// Baseline CSP that the block always enforces, regardless of `cfg.csp`.
///
/// `cfg.csp` directives are merged *on top of* this baseline rather than
/// replacing it — tenants can extend (add hashes/origins/etc.) but cannot
/// admit script from arbitrary origins, re-enable `unsafe-eval`, widen
/// `base-uri`/`form-action`, or touch `frame-ancestors` (see [`merge_csp`]).
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

/// Cross-origin-isolation posture for this deployment's responses. Drives
/// `Cross-Origin-Opener-Policy` and `Cross-Origin-Embedder-Policy`, the pair
/// that together make a document `crossOriginIsolated` (required for
/// `SharedArrayBuffer`, high-resolution timers, and similar APIs gated behind
/// isolation).
///
/// This is opt-in per deployment, not a new baseline default: per the HTML
/// spec, a document that sends a COEP other than `unsafe-none` can only embed
/// nested documents (`<iframe>`) that also carry a compatible COEP, so
/// turning this on can break framing of cross-origin content that hasn't
/// opted in itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrossOriginIsolation {
    /// Neither header is emitted (default). A no-op — this block never sent
    /// COOP/COEP before this knob existed, and `none` must stay that way
    /// rather than emitting an explicit `unsafe-none`.
    None,
    /// `Cross-Origin-Opener-Policy: same-origin` +
    /// `Cross-Origin-Embedder-Policy: credentialless`. Cross-origin `no-cors`
    /// subresources still load — they're fetched without credentials —
    /// without the third party needing to opt in via CORP/CORS.
    Credentialless,
    /// `Cross-Origin-Opener-Policy: same-origin` +
    /// `Cross-Origin-Embedder-Policy: require-corp`. Every cross-origin
    /// subresource must opt in via `Cross-Origin-Resource-Policy` or CORS, or
    /// it fails to load.
    RequireCorp,
}

impl CrossOriginIsolation {
    /// The `Cross-Origin-Embedder-Policy` value to emit, or `None` when the
    /// posture is [`Self::None`] and no isolation headers should be sent.
    fn coep_value(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Credentialless => Some("credentialless"),
            Self::RequireCorp => Some("require-corp"),
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
/// The CSP applied at request time is `merge_csp(DEFAULT_CSP, cfg.csp)` with
/// `frame-ancestors` set from `cfg.frame_ancestors`, composed once at Init.
/// Whatever the operator puts in `cfg.csp`, no script directive admits an
/// arbitrary origin or `'unsafe-eval'`; each refused directive or source is
/// logged at Init. See [`merge_csp`].
pub struct SecurityHeadersBlock {
    /// Init-composed policy, `frame-ancestors` included.
    csp: OnceLock<String>,
    /// Init-resolved `frame_ancestors` policy. Unset until Init parses the
    /// `frame_ancestors` config key; `effective_frame_ancestors` falls back
    /// to `FrameAncestors::None` (today's default) until then.
    frame_ancestors: OnceLock<FrameAncestors>,
    /// Init-resolved `cross_origin_isolation` policy. Unset until Init parses
    /// the `cross_origin_isolation` config key; `effective_cross_origin_isolation`
    /// falls back to `CrossOriginIsolation::None` (today's default — no
    /// COOP/COEP headers) until then.
    cross_origin_isolation: OnceLock<CrossOriginIsolation>,
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
    /// The first `Init` lifecycle event replaces the default with the
    /// operator `csp` config merged through [`merge_csp`] and the
    /// `frame_ancestors` config applied.
    pub fn new() -> Self {
        Self {
            csp: OnceLock::new(),
            frame_ancestors: OnceLock::new(),
            cross_origin_isolation: OnceLock::new(),
        }
    }

    /// The CSP applied to responses: the Init-composed value, or
    /// [`DEFAULT_CSP`] (whose `frame-ancestors 'none'` matches the
    /// [`FrameAncestors::None`] default) when Init has not (yet) run.
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

    /// The cross-origin-isolation posture applied to responses: the
    /// Init-set value, or [`CrossOriginIsolation::None`] (today's default —
    /// no COOP/COEP headers) when Init has not (yet) supplied one.
    fn effective_cross_origin_isolation(&self) -> CrossOriginIsolation {
        self.cross_origin_isolation
            .get()
            .copied()
            .unwrap_or(CrossOriginIsolation::None)
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
                 on top of the block's restrictive baseline (see merge_csp). \
                 Script sources that would admit any origin, a host wildcard, \
                 a non-https host or 'unsafe-eval', widenings of \
                 base-uri/form-action, a report-uri off this origin and \
                 frame-ancestors are refused and logged at Init.",
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
            ConfigVar::new(
                "cross_origin_isolation",
                "`none` (default: no Cross-Origin-Opener-Policy / Cross-Origin-Embedder-Policy \
                 headers) or `credentialless` / `require-corp`, both of which set \
                 Cross-Origin-Opener-Policy: same-origin and make the document \
                 crossOriginIsolated. `credentialless` keeps cross-origin no-cors \
                 subresources loadable — they're fetched without credentials — without the \
                 third party opting in; `require-corp` requires every cross-origin \
                 subresource to opt in via CORP or CORS. Per the HTML spec, a document \
                 sending either value can only embed nested documents that also carry a \
                 compatible COEP, so this is opt-in per deployment rather than a new \
                 baseline default.",
                "none",
            )
            .name("Cross-origin isolation"),
            ConfigVar::new(
                "allow_blob_workers",
                "`true` adds `blob:` to `worker-src`, and only there, so a page \
                 can start a worker from a blob URL it built (an in-browser \
                 toolchain spawning its own sub-workers). For an embedder's own \
                 step config: the operator `csp` value still cannot add `blob:` \
                 to any script directive. `false` (the default) leaves \
                 `worker-src` as the merged policy has it.",
                "false",
            )
            .name("Allow blob: workers"),
        ])
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let frame_ancestors = self.effective_frame_ancestors();
        let csp = self.effective_csp();
        let cross_origin_isolation = self.effective_cross_origin_isolation();

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
        // `none` stays a true no-op: this block never emitted COOP/COEP
        // before this knob existed, so the default must not start sending
        // `unsafe-none` (or any other value) on every response.
        if let Some(coep) = cross_origin_isolation.coep_value() {
            out_msg.set_meta("resp.header.Cross-Origin-Opener-Policy", "same-origin");
            out_msg.set_meta("resp.header.Cross-Origin-Embedder-Policy", coep);
        }

        OutputStream::continue_with(out_msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            let config = BlockConfig::from_event(&event);
            let frame_ancestors = match config.str_or("frame_ancestors", "none") {
                "self" => FrameAncestors::SelfOrigin,
                _ => FrameAncestors::None,
            };
            let allow_blob_workers = allow_blob_workers(&config)?;
            let custom_csp = config.str_or("csp", "");
            check_csp_characters(custom_csp)?;
            let merged = merge_csp(DEFAULT_CSP, custom_csp);
            for refusal in &merged.refused {
                tracing::warn!("security-headers: CSP config refused {refusal}");
            }
            let policy = if allow_blob_workers {
                with_blob_workers(&merged.policy)
            } else {
                merged.policy
            };
            // Write-once: Init fires a single time per registration.
            let _ = self.frame_ancestors.set(frame_ancestors);
            let _ = self.csp.set(with_frame_ancestors(&policy, frame_ancestors));
            match config.str_or("cross_origin_isolation", "none") {
                "credentialless" => {
                    let _ = self
                        .cross_origin_isolation
                        .set(CrossOriginIsolation::Credentialless);
                }
                "require-corp" => {
                    let _ = self
                        .cross_origin_isolation
                        .set(CrossOriginIsolation::RequireCorp);
                }
                _ => {
                    let _ = self.cross_origin_isolation.set(CrossOriginIsolation::None);
                }
            }
        }
        Ok(())
    }
}

/// Fail Init on a `csp` config character that cannot appear in a header
/// value: anything but visible ASCII and the ASCII whitespace that separates
/// tokens (which [`merge_csp`] re-serializes as single spaces). A pasted
/// smart quote or control character is a configuration mistake to fix, not
/// a source to drop.
fn check_csp_characters(csp: &str) -> std::result::Result<(), WaferError> {
    match csp
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_graphic() || c.is_ascii_whitespace()))
    {
        Some((at, c)) => Err(WaferError::new(
            ErrorCode::InvalidArgument,
            format!(
                "security-headers: `csp` config has {c:?} (U+{:04X}) at byte {at}; \
                 a Content-Security-Policy may only contain visible ASCII and spaces",
                u32::from(c),
            ),
        )),
        None => Ok(()),
    }
}

/// The `allow_blob_workers` step config: absent, `true` or `false` (a JSON
/// bool or its string spelling). Anything else fails Init rather than leave
/// the reader to guess which way a typo was meant.
fn allow_blob_workers(config: &BlockConfig) -> std::result::Result<bool, WaferError> {
    match config.get("allow_blob_workers") {
        None => Ok(false),
        Some(_) => config.bool("allow_blob_workers").ok_or_else(|| {
            WaferError::new(
                ErrorCode::InvalidArgument,
                "security-headers: `allow_blob_workers` must be true or false",
            )
        }),
    }
}

/// Add `blob:` to the `worker-src` directive of a [`merge_csp`] policy.
///
/// Without a `worker-src`, a browser takes worker sources from `child-src`,
/// then `script-src`, then `default-src`; the directive this adds starts from
/// that fallback, so the sources workers already had are kept and `blob:` is
/// the only addition. No other directive changes.
fn with_blob_workers(csp: &str) -> String {
    let mut directives: Vec<(String, Vec<String>)> = csp
        .split(';')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| {
            let mut tokens = d.split_ascii_whitespace();
            let name = tokens.next().unwrap_or_default().to_string();
            (name, tokens.map(str::to_string).collect())
        })
        .collect();
    let sources_of = |directives: &[(String, Vec<String>)], name: &str| {
        directives
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, sources)| sources.clone())
    };
    if let Some((_, sources)) = directives.iter_mut().find(|(n, _)| n == "worker-src") {
        if !sources.iter().any(|s| s == "blob:") {
            sources.push("blob:".to_string());
        }
    } else {
        let mut sources = ["child-src", "script-src", "default-src"]
            .iter()
            .find_map(|name| sources_of(&directives, name))
            .unwrap_or_default();
        sources.push("blob:".to_string());
        directives.push(("worker-src".to_string(), sources));
    }
    directives
        .into_iter()
        .map(|(name, sources)| {
            if sources.is_empty() {
                name
            } else {
                format!("{name} {}", sources.join(" "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Rewrite the `frame-ancestors` directive of a [`merge_csp`] policy (whose
/// directive names are lower-case and unique) to `fa`'s source, leaving
/// every other directive untouched. `frame_ancestors` is the only knob that
/// can change this directive — the operator `csp` config key cannot.
fn with_frame_ancestors(csp: &str, fa: FrameAncestors) -> String {
    csp.split(';')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| {
            if d.split_ascii_whitespace().next() == Some("frame-ancestors") {
                format!("frame-ancestors {}", fa.csp_source())
            } else {
                d.to_string()
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
        assert!(merged.refused.is_empty(), "{:?}", merged.refused);
        // Every baseline directive must still appear.
        assert!(merged.policy.contains("default-src 'self'"));
        assert!(merged.policy.contains("frame-ancestors 'none'"));
        assert!(merged.policy.contains("base-uri 'self'"));
    }

    #[test]
    fn merge_csp_strips_unsafe_eval_from_custom_script_src() {
        let merged = merge_csp(
            DEFAULT_CSP,
            "script-src 'self' 'unsafe-eval' https://cdn.example.com",
        );
        // unsafe-eval must be stripped, cdn must be added, baseline values kept.
        assert!(!merged.policy.contains("'unsafe-eval'"));
        assert!(merged.policy.contains("https://cdn.example.com"));
        assert!(merged.policy.contains("'unsafe-inline'")); // from baseline
    }

    #[test]
    fn merge_csp_strips_broad_script_sources_keeps_specific_host() {
        let merged = merge_csp(
            DEFAULT_CSP,
            "script-src https: data: https://cdn.example.com 'nonce-abc'",
        );
        let script = merged
            .policy
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
        assert!(merged.policy.contains("default-src 'self'"));
        // `*` must not appear as a default-src source.
        let default_section = merged
            .policy
            .split(';')
            .find(|s| s.trim().starts_with("default-src"))
            .unwrap_or("");
        assert!(!default_section.split_whitespace().any(|t| t == "*"));
    }

    #[test]
    fn merge_csp_allows_extension_with_new_directive() {
        let merged = merge_csp(DEFAULT_CSP, "media-src 'self' blob:");
        assert!(merged.policy.contains("media-src 'self' blob:"));
        // Baseline still intact.
        assert!(merged.policy.contains("frame-ancestors 'none'"));
    }

    // --- operator CSP through the block's Init -------------------------

    /// The `Content-Security-Policy` the block sends after Init with `config`.
    async fn served_csp(config: serde_json::Value) -> String {
        let block = SecurityHeadersBlock::new();
        block
            .lifecycle(&NoopCtx, init_event(&config.to_string()))
            .await
            .unwrap();
        let msg = block
            .handle(&NoopCtx, Message::new("retrieve:/"), InputStream::empty())
            .await
            .into_continue_message()
            .await
            .expect("middleware continues");
        msg.get_meta("resp.header.Content-Security-Policy")
            .to_string()
    }

    /// `(name, sources)` for every directive in `policy`, in order, with the
    /// name lower-cased as a browser reads it.
    fn directives(policy: &str) -> Vec<(String, Vec<&str>)> {
        policy
            .split(';')
            .filter_map(|d| {
                let mut tokens = d.split_ascii_whitespace();
                let name = tokens.next()?.to_ascii_lowercase();
                Some((name, tokens.collect()))
            })
            .collect()
    }

    /// The sources a browser enforces for `name`: CSP3 lower-cases directive
    /// names and ignores every occurrence after the first.
    fn enforced<'a>(policy: &'a str, name: &str) -> Option<Vec<&'a str>> {
        directives(policy)
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, sources)| sources)
    }

    #[tokio::test]
    async fn operator_csp_cannot_reenable_framing_or_admit_any_script_origin() {
        let csp = served_csp(serde_json::json!({
            "csp": "FRAME-ANCESTORS *; script-src https://*/; script-src-elem https:",
        }))
        .await;
        assert_eq!(
            enforced(&csp, "frame-ancestors"),
            Some(vec!["'none'"]),
            "{csp}"
        );
        for name in ["script-src", "script-src-elem"] {
            let sources = enforced(&csp, name).unwrap_or_default();
            assert!(
                !sources.contains(&"https://*/") && !sources.contains(&"https:"),
                "{name} admits any origin: {csp}"
            );
        }
    }

    #[tokio::test]
    async fn operator_csp_upper_case_script_src_cannot_replace_the_baseline() {
        let csp = served_csp(serde_json::json!({
            "csp": "SCRIPT-SRC https://*/ https://cdn.example.com",
        }))
        .await;
        assert_eq!(
            enforced(&csp, "script-src"),
            Some(vec!["'self'", "'unsafe-inline'", "https://cdn.example.com"]),
            "{csp}"
        );
    }

    #[tokio::test]
    async fn operator_csp_emits_each_directive_once() {
        let csp = served_csp(serde_json::json!({
            "csp": "Img-Src https://a.example; IMG-SRC https://b.example; Default-Src https://c.example",
        }))
        .await;
        let mut names: Vec<String> = directives(&csp).into_iter().map(|(n, _)| n).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "a directive is repeated: {csp}");
    }

    #[tokio::test]
    async fn operator_csp_wildcard_hosts_at_a_port_or_path_are_refused() {
        let csp = served_csp(serde_json::json!({
            "csp": "script-src https://*:443 *:443 */x; worker-src blob: *; \
                    script-src-attr https://*; default-src https://*/",
        }))
        .await;
        for name in [
            "script-src",
            "script-src-elem",
            "script-src-attr",
            "worker-src",
            "default-src",
        ] {
            for source in enforced(&csp, name).unwrap_or_default() {
                assert!(
                    !source.contains('*') && !source.ends_with(':'),
                    "{name} kept {source}: {csp}"
                );
            }
        }
    }

    /// The embedder knob adds `blob:` to `worker-src` and nothing else. With
    /// no `worker-src` in the policy the directive starts from its fallback
    /// (here the baseline `script-src`), so workers keep the sources they had.
    #[tokio::test]
    async fn allow_blob_workers_adds_blob_to_worker_src_only() {
        let off = served_csp(serde_json::json!({})).await;
        let on = served_csp(serde_json::json!({ "allow_blob_workers": true })).await;
        assert_eq!(
            enforced(&on, "worker-src"),
            Some(vec!["'self'", "'unsafe-inline'", "blob:"]),
            "{on}"
        );
        for name in [
            "script-src",
            "default-src",
            "child-src",
            "frame-src",
            "img-src",
        ] {
            assert_eq!(enforced(&on, name), enforced(&off, name), "{name}: {on}");
        }

        let with_own = served_csp(serde_json::json!({
            "allow_blob_workers": "true",
            "csp": "worker-src 'self'",
        }))
        .await;
        assert_eq!(
            enforced(&with_own, "worker-src"),
            Some(vec!["'self'", "blob:"]),
            "{with_own}"
        );
    }

    /// The operator `csp` string cannot reach what the knob grants: `blob:`
    /// in a worker or script directive is still refused, with the knob off.
    #[tokio::test]
    async fn operator_csp_cannot_add_blob_workers() {
        let csp = served_csp(serde_json::json!({
            "csp": "worker-src 'self' blob:; script-src blob:; child-src blob:",
        }))
        .await;
        for name in ["worker-src", "script-src", "child-src"] {
            assert!(
                !enforced(&csp, name).unwrap_or_default().contains(&"blob:"),
                "{name} admitted blob: from the operator csp: {csp}"
            );
        }
    }

    #[tokio::test]
    async fn allow_blob_workers_is_a_bool_or_init_fails() {
        let block = SecurityHeadersBlock::new();
        let err = block
            .lifecycle(
                &NoopCtx,
                init_event(&serde_json::json!({ "allow_blob_workers": "yes" }).to_string()),
            )
            .await
            .expect_err("a value that is not true or false must fail Init");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn frame_ancestors_knob_governs_the_directive_whatever_the_csp_says() {
        let csp = served_csp(serde_json::json!({
            "frame_ancestors": "self",
            "csp": "FRAME-ANCESTORS *",
        }))
        .await;
        assert_eq!(
            enforced(&csp, "frame-ancestors"),
            Some(vec!["'self'"]),
            "{csp}"
        );
    }

    #[tokio::test]
    async fn init_fails_on_a_csp_character_no_header_can_carry() {
        for (csp, named) in [
            ("script-src \u{2018}self\u{2019}", "U+2018"),
            ("img-src https://a.example\u{1}", "U+0001"),
            ("img-src\u{a0}https://a.example", "U+00A0"),
        ] {
            let block = SecurityHeadersBlock::new();
            let err = block
                .lifecycle(
                    &NoopCtx,
                    init_event(&serde_json::json!({ "csp": csp }).to_string()),
                )
                .await
                .expect_err("a non-header character must fail Init");
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{csp:?}");
            assert!(err.message.contains(named), "{csp:?}: {}", err.message);
        }
        // Newlines and tabs only separate tokens.
        let csp = served_csp(serde_json::json!({
            "csp": "img-src\n\thttps://a.example",
        }))
        .await;
        assert_eq!(
            enforced(&csp, "img-src"),
            Some(vec![
                "'self'",
                "data:",
                "blob:",
                "https:",
                "https://a.example"
            ]),
            "{csp}"
        );
    }

    #[tokio::test]
    async fn operator_csp_cannot_widen_form_action_or_base_uri() {
        let csp = served_csp(serde_json::json!({
            "csp": "form-action https:; base-uri https://evil.example",
        }))
        .await;
        assert_eq!(enforced(&csp, "form-action"), Some(vec!["'self'"]), "{csp}");
        assert_eq!(enforced(&csp, "base-uri"), Some(vec!["'self'"]), "{csp}");
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
        // Denies every access, as the trait's default `check_resource_access` does.
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_block::types::ResourceType,
            _access: wafer_block::types::ResourceAccess,
        ) -> bool {
            false
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
        assert!(
            merged.policy.contains("frame-ancestors 'none'"),
            "{}",
            merged.policy
        );
    }

    // --- cross_origin_isolation ------------------------------------------

    #[tokio::test]
    async fn cross_origin_isolation_defaults_to_none_and_omits_headers() {
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
        assert_eq!(msg.get_meta("resp.header.Cross-Origin-Opener-Policy"), "");
        assert_eq!(msg.get_meta("resp.header.Cross-Origin-Embedder-Policy"), "");
    }

    #[tokio::test]
    async fn cross_origin_isolation_credentialless_sets_coop_and_coep() {
        let block = SecurityHeadersBlock::new();
        block
            .lifecycle(
                &NoopCtx,
                init_event(r#"{"cross_origin_isolation":"credentialless"}"#),
            )
            .await
            .unwrap();
        let out = block
            .handle(&NoopCtx, Message::new("retrieve:/"), InputStream::empty())
            .await;
        let msg = out
            .into_continue_message()
            .await
            .expect("middleware continues");
        assert_eq!(
            msg.get_meta("resp.header.Cross-Origin-Opener-Policy"),
            "same-origin"
        );
        assert_eq!(
            msg.get_meta("resp.header.Cross-Origin-Embedder-Policy"),
            "credentialless"
        );
    }

    #[tokio::test]
    async fn cross_origin_isolation_require_corp_sets_coop_and_coep() {
        let block = SecurityHeadersBlock::new();
        block
            .lifecycle(
                &NoopCtx,
                init_event(r#"{"cross_origin_isolation":"require-corp"}"#),
            )
            .await
            .unwrap();
        let out = block
            .handle(&NoopCtx, Message::new("retrieve:/"), InputStream::empty())
            .await;
        let msg = out
            .into_continue_message()
            .await
            .expect("middleware continues");
        assert_eq!(
            msg.get_meta("resp.header.Cross-Origin-Opener-Policy"),
            "same-origin"
        );
        assert_eq!(
            msg.get_meta("resp.header.Cross-Origin-Embedder-Policy"),
            "require-corp"
        );
    }

    #[tokio::test]
    async fn cross_origin_isolation_unknown_value_treated_as_none() {
        let block = SecurityHeadersBlock::new();
        block
            .lifecycle(
                &NoopCtx,
                init_event(r#"{"cross_origin_isolation":"bogus"}"#),
            )
            .await
            .unwrap();
        let out = block
            .handle(&NoopCtx, Message::new("retrieve:/"), InputStream::empty())
            .await;
        let msg = out
            .into_continue_message()
            .await
            .expect("middleware continues");
        assert_eq!(msg.get_meta("resp.header.Cross-Origin-Opener-Policy"), "");
        assert_eq!(msg.get_meta("resp.header.Cross-Origin-Embedder-Policy"), "");
    }

    #[tokio::test]
    async fn cross_origin_isolation_does_not_affect_other_headers() {
        let block = SecurityHeadersBlock::new();
        block
            .lifecycle(
                &NoopCtx,
                init_event(r#"{"cross_origin_isolation":"require-corp"}"#),
            )
            .await
            .unwrap();
        let out = block
            .handle(&NoopCtx, Message::new("retrieve:/"), InputStream::empty())
            .await;
        let msg = out
            .into_continue_message()
            .await
            .expect("middleware continues");
        assert_eq!(
            msg.get_meta("resp.header.X-Content-Type-Options"),
            "nosniff"
        );
        assert_eq!(msg.get_meta("resp.header.X-Frame-Options"), "DENY");
        assert_eq!(
            msg.get_meta("resp.header.Referrer-Policy"),
            "strict-origin-when-cross-origin"
        );
        assert!(msg
            .get_meta("resp.header.Content-Security-Policy")
            .contains("frame-ancestors 'none'"));
        assert_eq!(
            msg.get_meta("resp.header.Strict-Transport-Security"),
            "max-age=31536000; includeSubDomains; preload"
        );
        assert_eq!(
            msg.get_meta("resp.header.Permissions-Policy"),
            "camera=(), microphone=(), geolocation=()"
        );
    }
}
