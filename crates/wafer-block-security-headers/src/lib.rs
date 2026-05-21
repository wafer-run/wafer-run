//! Security-headers block — appends a baseline of HTTP response headers
//! (Content-Security-Policy, Strict-Transport-Security, X-Content-Type-Options,
//! X-Frame-Options, Referrer-Policy, Permissions-Policy) to every message
//! that passes through it. The CSP is tenant-configurable via the `csp`
//! [`ConfigVar`] but the operator-supplied directives are merged on top of
//! a restrictive baseline (see [`merge_csp`]) so they can only widen the
//! policy in safe ways.

#![warn(missing_docs)]

use std::sync::RwLock;

use wafer_block::*;

/// Baseline CSP that the block always enforces, regardless of `cfg.csp`.
///
/// `cfg.csp` directives are merged *on top of* this baseline rather than
/// replacing it — tenants can extend (add hashes/origins/etc.) but cannot
/// weaken `default-src` to `*` or re-enable `unsafe-eval`.
const DEFAULT_CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self' https:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'";

/// SecurityHeadersBlock adds standard security headers to responses.
///
/// CSP is configurable via `block_config` — the runtime serializes the
/// config JSON to bytes and passes them in at `lifecycle(Init)`. Until
/// Init runs, the block uses the restrictive `DEFAULT_CSP`. Store via
/// `RwLock<String>` because `handle` takes `&self` and the config is
/// written once at Init, read on every request.
///
/// The CSP applied at request time is `merge_csp(DEFAULT_CSP, cfg.csp)`,
/// which guarantees:
/// * `default-src` never widens past the baseline (no `*`),
/// * `script-src` never gains `'unsafe-eval'`,
///
/// regardless of what the operator puts in `cfg.csp`. See `merge_csp`.
pub struct SecurityHeadersBlock {
    csp: RwLock<String>,
}

impl Default for SecurityHeadersBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl SecurityHeadersBlock {
    /// Build a new block seeded with the restrictive [`DEFAULT_CSP`].
    ///
    /// Any operator-supplied `csp` config will replace the seeded value
    /// (after merging through [`merge_csp`]) the first time the runtime
    /// fires the `Init` lifecycle event.
    pub fn new() -> Self {
        Self {
            csp: RwLock::new(DEFAULT_CSP.to_string()),
        }
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
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
        .flow_config(vec![ConfigVar::new(
            "csp",
            "Operator-supplied Content-Security-Policy directives, merged \
             on top of the block's restrictive baseline (see merge_csp).",
            "",
        )
        .name("CSP")])
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let csp = self
            .csp
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|_| DEFAULT_CSP.to_string());

        let mut out_msg = msg;
        out_msg.set_meta("resp.header.X-Content-Type-Options", "nosniff");
        out_msg.set_meta("resp.header.X-Frame-Options", "DENY");
        // SEC-085: X-XSS-Protection is deprecated. The legacy IE filter it
        // toggled was removed from modern browsers and can introduce XSS
        // in some configurations; CSP is the modern replacement. Header
        // intentionally omitted.
        out_msg.set_meta(
            "resp.header.Referrer-Policy",
            "strict-origin-when-cross-origin",
        );
        out_msg.set_meta("resp.header.Content-Security-Policy", &csp);
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
        if let LifecycleType::Init = event.event_type {
            if let Ok(cfg) = serde_json::from_slice::<serde_json::Value>(&event.data) {
                if let Some(custom_csp) = cfg.get("csp").and_then(|v| v.as_str()) {
                    let merged = merge_csp(DEFAULT_CSP, custom_csp);
                    if let Ok(mut guard) = self.csp.write() {
                        *guard = merged;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Merge `custom` into `baseline` directive-by-directive.
///
/// Both inputs are standard `directive value...; directive value...` CSP
/// strings. The result preserves every baseline directive (so the operator
/// cannot remove `frame-ancestors 'none'` or similar) and then applies the
/// custom values per directive subject to **non-weakening rules**:
///
/// * `default-src` — sources `*`, `http:`, `https:` are dropped; `'self'`
///   is always re-added if missing.
/// * `script-src` — `'unsafe-eval'` is always stripped; `*` is dropped.
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
            if is_default_src && matches!(lower.as_str(), "*" | "http:" | "https:") {
                continue;
            }
            if is_script_src && lower == "'unsafe-eval'" {
                continue;
            }
            if is_script_src && src == "*" {
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
        sources.retain(|s| {
            let l = s.to_lowercase();
            l != "*" && l != "http:" && l != "https:"
        });
    }
    if let Some(sources) = merged.get_mut("script-src") {
        sources.retain(|s| s.to_lowercase() != "'unsafe-eval'" && s != "*");
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
}
