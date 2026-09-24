#![warn(missing_docs)]
//! CORS middleware block for the WAFER runtime.
//!
//! Implements RFC 6454 cross-origin request handling: parses the request
//! `Origin` header, consults a configured allow-list, and sets the
//! `Access-Control-Allow-*` response headers (plus `Vary: Origin` whenever
//! an allow-list is configured) on the outgoing message. OPTIONS preflight requests short-circuit with
//! a 204 via [`OutputStream::halt`].
//!
//! See the [`CorsBlock`] type for the configuration contract and the
//! fail-closed behavior introduced by SEC-087/SEC-088.

use std::sync::OnceLock;

use wafer_block::*;

/// Default `Access-Control-Max-Age` (seconds) — how long a browser may cache
/// a CORS preflight result. 86400 = 24 hours.
///
/// Single source of truth for the `max_age` default: rendered into the
/// `max_age` [`ConfigVar`] and used as the per-request fallback in
/// [`CorsBlock::handle`].
const DEFAULT_MAX_AGE_SECONDS: u32 = 86_400;

/// Default `Access-Control-Allow-Methods` value.
///
/// Single source of truth for the `allowed_methods` default: rendered into
/// the `allowed_methods` [`ConfigVar`] and used as the per-request fallback
/// in [`CorsBlock::handle`].
const DEFAULT_ALLOWED_METHODS: &str = "GET, POST, PUT, PATCH, DELETE, OPTIONS";

/// Default `Access-Control-Allow-Headers` value.
///
/// Single source of truth for the `allowed_headers` default: rendered into
/// the `allowed_headers` [`ConfigVar`] and used as the per-request fallback
/// in [`CorsBlock::handle`].
const DEFAULT_ALLOWED_HEADERS: &str = "Content-Type, Authorization, X-Requested-With";

/// Whether a fail-closed (SEC-087, `allowed_origins` unset) request warrants
/// a denial warning. Same-origin and non-browser requests carry no `Origin`
/// header, so nothing is being denied and a warning would just spam the log
/// on every ordinary request. Only a real cross-origin request — one that
/// presents an `Origin` — is actually denied here and worth surfacing.
fn unconfigured_denial_is_loggable(origin: &str) -> bool {
    !origin.is_empty()
}

/// How a request's `Origin` matched the allow-list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OriginMatch {
    /// The origin is listed by name.
    Listed,
    /// The allow-list is `*`.
    Wildcard,
}

/// Match a request's `origin` against `allowed` (`*` or a comma-separated
/// list). `None` when the request has no `Origin` or the origin is not
/// allowed; the caller then emits no `Access-Control-Allow-Origin`.
/// Add `Origin` to the response's `Vary`, keeping whatever an earlier block
/// already varies on. `Vary` is a list, and adapters send one value per
/// header name, so every `resp.header.{vary}` entry in any case is folded
/// into a single `resp.header.Vary`. A `*` already varies on everything and
/// is left alone.
fn vary_on_origin(msg: &mut Message) {
    let is_vary = |key: &str| {
        key.strip_prefix(META_RESP_HEADER_PREFIX)
            .is_some_and(|name| name.eq_ignore_ascii_case("vary"))
    };
    let mut fields: Vec<String> = msg
        .meta
        .iter()
        .filter(|m| is_vary(&m.key))
        .flat_map(|m| m.value.split(','))
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(String::from)
        .collect();
    if !fields
        .iter()
        .any(|f| f == "*" || f.eq_ignore_ascii_case("origin"))
    {
        fields.push("Origin".to_string());
    }
    msg.meta.retain(|m| !is_vary(&m.key));
    msg.set_meta(format!("{META_RESP_HEADER_PREFIX}Vary"), fields.join(", "));
}

fn match_origin(allowed: &str, origin: &str) -> Option<OriginMatch> {
    if origin.is_empty() {
        None
    } else if allowed.trim() == "*" {
        Some(OriginMatch::Wildcard)
    } else if allowed.split(',').any(|o| o.trim() == origin) {
        Some(OriginMatch::Listed)
    } else {
        None
    }
}

/// CorsBlock handles CORS preflight and sets CORS headers.
///
/// # Configuration
///
/// `allowed_origins` must be set either via block config (parsed at
/// `lifecycle(Init)`) or via per-request `ctx.config_get("allowed_origins")`
/// (e.g. from a flow step). It is `*` or a comma-separated list of origins.
///
/// If neither path supplies a value, the block fails closed (SEC-087): Init
/// logs a warning but succeeds, since a flow step may still supply the list
/// per request, and `handle()` denies every cross-origin request (no
/// `Access-Control-Allow-Origin` header emitted).
///
/// # Access-Control-Allow-Origin
///
/// The header is only ever the request's own `Origin`, and only when that
/// origin is allowed: listed, or any origin under `*`. A request without an
/// `Origin` gets none. Credentials are allowed only for a listed origin.
///
/// # Vary: Origin
///
/// Whenever an allow-list is configured the response depends on the
/// request's `Origin` — including a request without one, or from an origin
/// that is refused — so the block sets `Vary: Origin` on every such
/// response. Without it, an intermediary cache can serve a response keyed
/// for one origin to a request from another — see SEC-088. `Origin` is
/// added to any `Vary` an earlier block set, never in place of it.
pub struct CorsBlock {
    /// Allow-list resolved at `Init` lifecycle, used as fallback when the
    /// per-request context does not supply `allowed_origins`. Unset until
    /// Init parses a non-empty value (write-once).
    allowed_origins: OnceLock<String>,
}

impl Default for CorsBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl CorsBlock {
    /// Construct a `CorsBlock` with no allow-list resolved. Methods, headers,
    /// and max-age fall back to [`DEFAULT_ALLOWED_METHODS`],
    /// [`DEFAULT_ALLOWED_HEADERS`], and [`DEFAULT_MAX_AGE_SECONDS`] unless
    /// overridden per request via `ctx.config_get`. The allow-list stays
    /// unset — and the block therefore fails closed on cross-origin
    /// requests — until `lifecycle(Init)` parses block config or per-request
    /// `ctx.config_get` supplies one. See SEC-087 for the rationale.
    pub fn new() -> Self {
        Self {
            allowed_origins: OnceLock::new(),
        }
    }

    /// Borrow the Init-cached allow-list without cloning. Returns `None` until
    /// `lifecycle(Init)` has stored a value (the fail-closed state).
    fn cached_origins(&self) -> Option<&str> {
        self.allowed_origins.get().map(String::as_str)
    }
}

#[wafer_async_trait]
impl Block for CorsBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/cors",
            "0.0.1",
            "middleware@v1",
            "CORS preflight handler and header injection",
        )
        .infrastructure()
        .flow_config(vec![
            ConfigVar::new(
                "allowed_origins",
                "Origins permitted to make cross-origin requests. Comma-separated \
                 string (e.g. \"https://a,https://b\" or \"*\") or JSON array of \
                 strings (e.g. [\"*\"]). No default: when unset the block fails \
                 closed and denies all cross-origin requests (SEC-087).",
                "",
            )
            .name("Allowed Origins"),
            ConfigVar::new(
                "allowed_methods",
                "Comma-separated list of HTTP methods returned in \
                 Access-Control-Allow-Methods.",
                DEFAULT_ALLOWED_METHODS,
            )
            .name("Allowed Methods"),
            ConfigVar::new(
                "allowed_headers",
                "Comma-separated list of request headers returned in \
                 Access-Control-Allow-Headers.",
                DEFAULT_ALLOWED_HEADERS,
            )
            .name("Allowed Headers"),
            ConfigVar::new(
                "max_age",
                "Seconds a browser may cache the CORS preflight result, \
                 returned in Access-Control-Max-Age.",
                &DEFAULT_MAX_AGE_SECONDS.to_string(),
            )
            .name("Max Age (seconds)"),
        ])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        // Resolve allow-list: per-request config > Init-cached > deny.
        // No wildcard default — failing closed is the point. Both sources
        // are borrowed as `&str`: `allowed` is only compared and split
        // below, never stored or sent.
        let origins: Option<&str> = ctx
            .config_get("allowed_origins")
            .or_else(|| self.cached_origins());

        let methods = ctx
            .config_get("allowed_methods")
            .unwrap_or(DEFAULT_ALLOWED_METHODS)
            .to_string();
        let headers = ctx
            .config_get("allowed_headers")
            .unwrap_or(DEFAULT_ALLOWED_HEADERS)
            .to_string();
        let max_age = ctx
            .config_get("max_age")
            .map_or_else(|| DEFAULT_MAX_AGE_SECONDS.to_string(), |s| s.to_string());

        let mut out_msg = msg;

        let origin = out_msg.header("Origin").to_string();
        let mut credentials = false;

        match origins {
            None => {
                // SEC-087: no configuration → deny cross-origin. We do NOT
                // emit Access-Control-Allow-Origin; same-origin requests are
                // unaffected. Warn only for genuinely-denied cross-origin
                // requests — see `unconfigured_denial_is_loggable`.
                if unconfigured_denial_is_loggable(&origin) {
                    tracing::warn!(
                        "CORS: allowed_origins unconfigured — denying cross-origin request \
                         (set `allowed_origins` in the block config or per-flow-step config)",
                    );
                }
            }
            Some(allowed) => {
                // SEC-088: with an allow-list the response depends on the
                // request's `Origin` — present, absent or refused — so a
                // cache must key on it.
                vary_on_origin(&mut out_msg);
                match match_origin(allowed, &origin) {
                    Some(OriginMatch::Listed) => {
                        // Origin explicitly in allowlist: safe to enable credentials.
                        out_msg.set_meta("resp.header.Access-Control-Allow-Origin", &origin);
                        credentials = true;
                    }
                    Some(OriginMatch::Wildcard) => {
                        // Credentials MUST stay false under a wildcard, per spec.
                        out_msg.set_meta("resp.header.Access-Control-Allow-Origin", &origin);
                    }
                    // No `Origin` (same-origin or non-browser) or a refused
                    // one: emit nothing, and the browser blocks a cross-origin read.
                    None => {}
                }
            }
        }

        out_msg.set_meta("resp.header.Access-Control-Allow-Methods", &methods);
        out_msg.set_meta("resp.header.Access-Control-Allow-Headers", &headers);
        if credentials {
            out_msg.set_meta("resp.header.Access-Control-Allow-Credentials", "true");
        }
        out_msg.set_meta("resp.header.Access-Control-Max-Age", &max_age);

        // Handle OPTIONS preflight — respond with empty 204 + CORS headers.
        // Uses Halt (not Drop) so the flow executor short-circuits AND the
        // CORS meta we just set on out_msg actually reaches the HTTP wire.
        // See spec
        // docs/superpowers/specs/2026-05-25-wave-13-halt-terminal-and-validator-escalation-design.md
        if out_msg.get_meta("http.method") == "OPTIONS" {
            out_msg.set_meta("resp.status", "204");
            return OutputStream::halt(Vec::new(), out_msg.meta);
        }

        OutputStream::continue_with(out_msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            // Warn on unknown config keys before parsing. Catches typos
            // like `allow_origins` (vs the declared `allowed_origins`)
            // at deploy time — would have caught the Wave 8/9 regression.
            let info = self.info();
            let unknown = wafer_block::validation::unknown_flow_config_keys(&info, &event.data);
            if !unknown.is_empty() {
                let declared: Vec<&str> = info.flow_config.iter().map(|v| v.key.as_str()).collect();
                tracing::warn!(
                    "CORS: unknown config key(s) {:?} — declared keys are {:?}; \
                     did you mean one of these?",
                    unknown,
                    declared,
                );
            }

            // Parse block config if any was supplied. Accept either:
            //   - string  -> use as-is (canonical comma-separated form)
            //   - array of strings -> join with `,` (matches the internal
            //     split-on-comma representation)
            //   - anything else -> warn + fail closed (SEC-087)
            let config = BlockConfig::from_event(&event);
            let cfg_origins = match config.get("allowed_origins") {
                Some(serde_json::Value::String(s)) => Some(s.clone()),
                Some(serde_json::Value::Array(items)) => {
                    let strs: Vec<&str> = items.iter().filter_map(|v| v.as_str()).collect();
                    if strs.is_empty() {
                        tracing::warn!(
                            "CORS: `allowed_origins` array is empty or \
                             contains no string entries — falling back to \
                             fail-closed (set a non-empty array of origin \
                             strings, or a comma-separated string)",
                        );
                        None
                    } else {
                        if strs.len() != items.len() {
                            tracing::warn!(
                                "CORS: `allowed_origins` array contains {} non-string \
                                 entry/entries (out of {}) which were dropped — \
                                 only string entries are honored",
                                items.len() - strs.len(),
                                items.len(),
                            );
                        }
                        Some(strs.join(","))
                    }
                }
                Some(other) => {
                    tracing::warn!(
                        "CORS: `allowed_origins` has unsupported JSON shape \
                         ({:?}) — expected string or array of strings; falling \
                         back to fail-closed",
                        other,
                    );
                    None
                }
                None => None,
            };

            match cfg_origins {
                Some(v) if !v.trim().is_empty() => {
                    // Write-once: Init fires a single time per registration.
                    let _ = self.allowed_origins.set(v);
                    Ok(())
                }
                _ => {
                    // SEC-087: fail closed at startup when allowed_origins is
                    // unconfigured AND no per-flow-step config will supply
                    // one. We can't tell at Init whether downstream steps
                    // will inject `allowed_origins`, so the contract is:
                    // *either* set it on the block config, *or* set it on
                    // every flow step that uses this block. Block-level
                    // config is the supported path; the warning here points
                    // to it.
                    //
                    // We do not return an error from Init because some
                    // deployments legitimately configure CORS per flow
                    // step. `handle()` will still deny cross-origin
                    // requests if neither config is present.
                    tracing::warn!(
                        "CORS: `allowed_origins` not set at Init — every flow step that \
                         uses wafer-run/cors must supply `allowed_origins` in its step \
                         config, otherwise cross-origin requests are denied.",
                    );
                    Ok(())
                }
            }
        } else {
            Ok(())
        }
    }
}

wafer_block::register_static_block!("wafer-run/cors", CorsBlock);

#[cfg(test)]
mod tests {
    use wafer_block::streams::output::TerminalNotResponse;

    use super::*;

    #[test]
    fn declared_config_defaults_match_runtime_fallbacks() {
        let info = CorsBlock::new().info();
        let default_of = |key: &str| {
            info.flow_config
                .iter()
                .find(|v| v.key == key)
                .map(|v| v.default.clone())
                .expect("ConfigVar must be declared")
        };
        // SEC-087: no permissive declared default — empty means fail closed.
        assert_eq!(default_of("allowed_origins"), "");
        assert_eq!(default_of("allowed_methods"), DEFAULT_ALLOWED_METHODS);
        assert_eq!(default_of("allowed_headers"), DEFAULT_ALLOWED_HEADERS);
        assert_eq!(default_of("max_age"), DEFAULT_MAX_AGE_SECONDS.to_string());
    }

    #[test]
    fn block_constructor_has_no_default_allowed_origins() {
        let block = CorsBlock::new();
        // SEC-087: default must NOT be "*" — the block must fail closed
        // until explicitly configured.
        assert!(
            block.cached_origins().is_none(),
            "CorsBlock must not default to a permissive allowed_origins value",
        );
    }

    #[tokio::test]
    async fn init_with_array_origins_populates_cache() {
        let block = CorsBlock::new();
        let cfg = serde_json::json!({
            "allowed_origins": ["https://a.example", "https://b.example"],
        });
        let event = LifecycleEvent {
            event_type: LifecycleType::Init,
            data: serde_json::to_vec(&cfg).expect("json"),
        };
        let ctx = TestContext::new();
        block.lifecycle(&ctx, event).await.expect("init ok");
        assert_eq!(
            block.cached_origins(),
            Some("https://a.example,https://b.example"),
        );
    }

    #[tokio::test]
    async fn init_with_wildcard_array_populates_cache() {
        let block = CorsBlock::new();
        let cfg = serde_json::json!({ "allowed_origins": ["*"] });
        let event = LifecycleEvent {
            event_type: LifecycleType::Init,
            data: serde_json::to_vec(&cfg).expect("json"),
        };
        let ctx = TestContext::new();
        block.lifecycle(&ctx, event).await.expect("init ok");
        assert_eq!(block.cached_origins(), Some("*"));
    }

    #[tokio::test]
    async fn init_with_empty_array_leaves_cache_unset() {
        let block = CorsBlock::new();
        let cfg = serde_json::json!({ "allowed_origins": [] });
        let event = LifecycleEvent {
            event_type: LifecycleType::Init,
            data: serde_json::to_vec(&cfg).expect("json"),
        };
        let ctx = TestContext::new();
        block.lifecycle(&ctx, event).await.expect("init ok");
        assert!(
            block.cached_origins().is_none(),
            "empty array must fall through to SEC-087 fail-closed path",
        );
    }

    #[tokio::test]
    async fn init_with_object_origins_leaves_cache_unset() {
        let block = CorsBlock::new();
        let cfg = serde_json::json!({
            "allowed_origins": { "unexpected": "object" },
        });
        let event = LifecycleEvent {
            event_type: LifecycleType::Init,
            data: serde_json::to_vec(&cfg).expect("json"),
        };
        let ctx = TestContext::new();
        block.lifecycle(&ctx, event).await.expect("init ok");
        assert!(block.cached_origins().is_none());
    }

    #[tokio::test]
    async fn init_with_mixed_array_keeps_strings_and_drops_others() {
        let block = CorsBlock::new();
        let cfg = serde_json::json!({
            "allowed_origins": [42, "https://a.example", true, "https://b.example"],
        });
        let event = LifecycleEvent {
            event_type: LifecycleType::Init,
            data: serde_json::to_vec(&cfg).expect("json"),
        };
        let ctx = TestContext::new();
        block.lifecycle(&ctx, event).await.expect("init ok");
        assert_eq!(
            block.cached_origins(),
            Some("https://a.example,https://b.example"),
            "string entries should be preserved when non-string entries are dropped",
        );
    }

    #[tokio::test]
    async fn init_with_unknown_key_still_parses_known_keys() {
        // The well-declared `allowed_origins` must still populate the
        // cache; the typo'd `allow_origins` is ignored (warning is a
        // side-effect we don't assert on directly — observed via
        // tracing::warn!).
        let block = CorsBlock::new();
        let cfg = serde_json::json!({
            "allow_origins": "https://typo.example",
            "allowed_origins": "https://real.example",
        });
        let event = LifecycleEvent {
            event_type: LifecycleType::Init,
            data: serde_json::to_vec(&cfg).expect("json"),
        };
        let ctx = TestContext::new();
        block.lifecycle(&ctx, event).await.expect("init ok");
        assert_eq!(
            block.cached_origins(),
            Some("https://real.example"),
            "the correctly-declared key must still populate the cache \
             when an unknown key is also present",
        );
    }

    // Minimal Context shim — CorsBlock's lifecycle Init doesn't touch ctx.
    #[derive(Clone)]
    struct TestContext {
        allowed_origins: Option<String>,
    }

    impl TestContext {
        fn new() -> Self {
            Self {
                allowed_origins: None,
            }
        }

        fn with_origin(origin: &str) -> Self {
            Self {
                allowed_origins: Some(origin.to_string()),
            }
        }
    }

    #[wafer_async_trait]
    impl Context for TestContext {
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
        fn config_get(&self, key: &str) -> Option<&str> {
            if key == "allowed_origins" {
                self.allowed_origins.as_deref()
            } else {
                None
            }
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

    #[tokio::test]
    async fn options_preflight_emits_halt_with_cors_meta() {
        let ctx = TestContext::with_origin("https://example.com");
        let block = CorsBlock::new();
        // Init the block with an allowlist that includes our test origin.
        block
            .lifecycle(
                &ctx,
                LifecycleEvent {
                    event_type: LifecycleType::Init,
                    data: serde_json::to_vec(&serde_json::json!({
                        "allowed_origins": "https://example.com",
                    }))
                    .unwrap(),
                },
            )
            .await
            .unwrap();

        let mut msg = Message::new("http.request");
        msg.set_meta("http.method", "OPTIONS");
        msg.set_meta("http.header.origin", "https://example.com");

        let output = block.handle(&ctx, msg, InputStream::empty()).await;
        let buf = output.collect_buffered().await;

        match buf {
            Err(TerminalNotResponse::Halt(resp)) => {
                assert!(
                    resp.body.is_empty(),
                    "OPTIONS preflight body should be empty"
                );
                let meta_has =
                    |k: &str, v: &str| resp.meta.iter().any(|m| m.key == k && m.value == v);
                let meta_has_key = |k: &str| resp.meta.iter().any(|m| m.key == k);
                assert!(meta_has("resp.status", "204"), "missing resp.status=204");
                assert!(
                    meta_has(
                        "resp.header.Access-Control-Allow-Origin",
                        "https://example.com"
                    ),
                    "missing or wrong Access-Control-Allow-Origin"
                );
                assert!(
                    meta_has_key("resp.header.Access-Control-Allow-Methods"),
                    "missing Access-Control-Allow-Methods"
                );
                assert!(
                    meta_has_key("resp.header.Access-Control-Allow-Headers"),
                    "missing Access-Control-Allow-Headers"
                );
                assert!(
                    meta_has_key("resp.header.Access-Control-Max-Age"),
                    "missing Access-Control-Max-Age"
                );
                assert!(
                    meta_has("resp.header.Vary", "Origin"),
                    "missing Vary: Origin (SEC-088)"
                );
            }
            other => panic!("expected Err(Halt), got {other:?}"),
        }
    }

    /// Init `CorsBlock` with `allowed_origins`, then run one request carrying
    /// `origin` (none when empty) through `handle` and return its meta.
    async fn cors_meta(allowed_origins: Option<&str>, origin: &str) -> Vec<(String, String)> {
        let block = CorsBlock::new();
        let cfg = match allowed_origins {
            Some(v) => serde_json::json!({ "allowed_origins": v }),
            None => serde_json::json!({}),
        };
        block
            .lifecycle(
                &TestContext::new(),
                LifecycleEvent {
                    event_type: LifecycleType::Init,
                    data: serde_json::to_vec(&cfg).expect("json"),
                },
            )
            .await
            .expect("init ok");
        let mut msg = Message::new("http.request");
        msg.set_meta("http.method", "GET");
        if !origin.is_empty() {
            msg.set_meta("http.header.origin", origin);
        }
        match block
            .handle(&TestContext::new(), msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Continue(msg)) => {
                msg.meta.into_iter().map(|m| (m.key, m.value)).collect()
            }
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    fn meta_value<'a>(meta: &'a [(String, String)], key: &str) -> Option<&'a str> {
        meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    const TWO_ORIGINS: &str = "https://a.example, https://b.example";

    #[tokio::test]
    async fn a_request_without_origin_gets_no_allow_origin_but_varies() {
        let meta = cors_meta(Some(TWO_ORIGINS), "").await;
        assert_eq!(
            meta_value(&meta, "resp.header.Access-Control-Allow-Origin"),
            None,
            "{meta:?}"
        );
        assert_eq!(
            meta_value(&meta, "resp.header.Vary"),
            Some("Origin"),
            "{meta:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_origin_gets_no_allow_origin_but_varies() {
        let meta = cors_meta(Some(TWO_ORIGINS), "https://evil.example").await;
        assert_eq!(
            meta_value(&meta, "resp.header.Access-Control-Allow-Origin"),
            None,
            "{meta:?}"
        );
        assert_eq!(
            meta_value(&meta, "resp.header.Vary"),
            Some("Origin"),
            "{meta:?}"
        );
    }

    #[tokio::test]
    async fn a_listed_origin_is_reflected_alone_with_credentials() {
        let meta = cors_meta(Some(TWO_ORIGINS), "https://b.example").await;
        assert_eq!(
            meta_value(&meta, "resp.header.Access-Control-Allow-Origin"),
            Some("https://b.example"),
            "{meta:?}"
        );
        assert_eq!(
            meta_value(&meta, "resp.header.Access-Control-Allow-Credentials"),
            Some("true"),
            "{meta:?}"
        );
        assert_eq!(
            meta_value(&meta, "resp.header.Vary"),
            Some("Origin"),
            "{meta:?}"
        );
    }

    #[tokio::test]
    async fn a_wildcard_reflects_the_origin_without_credentials() {
        let meta = cors_meta(Some("*"), "https://any.example").await;
        assert_eq!(
            meta_value(&meta, "resp.header.Access-Control-Allow-Origin"),
            Some("https://any.example"),
            "{meta:?}"
        );
        assert_eq!(
            meta_value(&meta, "resp.header.Access-Control-Allow-Credentials"),
            None,
            "{meta:?}"
        );
        assert_eq!(
            meta_value(&meta, "resp.header.Vary"),
            Some("Origin"),
            "{meta:?}"
        );
    }

    #[tokio::test]
    async fn an_unconfigured_block_sends_neither_allow_origin_nor_vary() {
        // Fail closed: no allow-list means nothing depends on `Origin`.
        let meta = cors_meta(None, "https://a.example").await;
        assert_eq!(
            meta_value(&meta, "resp.header.Access-Control-Allow-Origin"),
            None,
            "{meta:?}"
        );
        assert_eq!(meta_value(&meta, "resp.header.Vary"), None, "{meta:?}");
    }

    #[test]
    fn unconfigured_denial_is_silent_for_same_origin_requests() {
        // No `Origin` header (same-origin or non-browser): nothing is denied,
        // so it must not warn — otherwise every ordinary request spams the log.
        assert!(!unconfigured_denial_is_loggable(""));
    }

    #[test]
    fn unconfigured_denial_warns_for_real_cross_origin_requests() {
        assert!(unconfigured_denial_is_loggable("https://cross.example"));
    }

    /// An earlier block's `Vary` (here `Accept-Encoding`, in lower case) is
    /// extended, not replaced — replacing it lets a cache serve a
    /// compressed body to a client that cannot decode it.
    #[tokio::test]
    async fn vary_extends_an_earlier_value() {
        let block = CorsBlock::new();
        block
            .lifecycle(
                &TestContext::new(),
                LifecycleEvent {
                    event_type: LifecycleType::Init,
                    data: serde_json::to_vec(
                        &serde_json::json!({ "allowed_origins": TWO_ORIGINS }),
                    )
                    .expect("json"),
                },
            )
            .await
            .expect("init ok");
        for (earlier, expected) in [
            ("Accept-Encoding", "Accept-Encoding, Origin"),
            ("accept-encoding, origin", "accept-encoding, origin"),
            ("*", "*"),
        ] {
            let mut msg = Message::new("http.request");
            msg.set_meta("http.method", "GET");
            msg.set_meta("http.header.origin", "https://a.example");
            msg.set_meta("resp.header.vary", earlier);
            let meta: Vec<(String, String)> = match block
                .handle(&TestContext::new(), msg, InputStream::empty())
                .await
                .collect_buffered()
                .await
            {
                Err(TerminalNotResponse::Continue(msg)) => {
                    msg.meta.into_iter().map(|m| (m.key, m.value)).collect()
                }
                other => panic!("expected Continue, got {other:?}"),
            };
            let varies: Vec<&str> = meta
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("resp.header.vary"))
                .map(|(_, v)| v.as_str())
                .collect();
            assert_eq!(varies, vec![expected], "{earlier}: {meta:?}");
        }
    }
}
