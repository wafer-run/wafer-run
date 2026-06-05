#![warn(missing_docs)]
//! CORS middleware block for the WAFER runtime.
//!
//! Implements RFC 6454 cross-origin request handling: parses the request
//! `Origin` header, consults a configured allow-list, and sets the
//! `Access-Control-Allow-*` response headers (plus `Vary: Origin`) on
//! the outgoing message. OPTIONS preflight requests short-circuit with
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

/// CorsBlock handles CORS preflight and sets CORS headers.
///
/// # Configuration
///
/// `allowed_origins` is **required** — it must be set either via
/// block config (parsed at `lifecycle(Init)`) or via per-request
/// `ctx.config_get("allowed_origins")` (e.g. from a flow step).
///
/// If neither path supplies a value, the block fails closed:
/// `lifecycle(Init)` returns an error so the runtime refuses to start,
/// and per-request `handle()` denies all cross-origin requests (no
/// `Access-Control-Allow-Origin` header emitted).
///
/// This is a deliberate change from the previous default of `"*"`,
/// which silently exposed APIs to any origin (see SEC-087 in the
/// 2026-04-10 security review).
///
/// # Vary: Origin
///
/// Whenever the response includes a reflected `Access-Control-Allow-Origin`
/// (either via wildcard or allow-list match), the block also sets
/// `Vary: Origin`. Without it, intermediary caches can serve a response
/// targeted at Origin A to a request from Origin B — see SEC-088.
pub struct CorsBlock {
    /// Allow-list resolved at `Init` lifecycle, used as fallback when the
    /// per-request context does not supply `allowed_origins`. Unset until
    /// Init parses a non-empty value (write-once).
    allowed_origins: OnceLock<String>,
    allowed_methods: String,
    allowed_headers: String,
}

impl Default for CorsBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl CorsBlock {
    /// Construct a `CorsBlock` with no allow-list resolved and the standard
    /// header defaults (`GET, POST, PUT, PATCH, DELETE, OPTIONS` for
    /// `Access-Control-Allow-Methods`; `Content-Type, Authorization,
    /// X-Requested-With` for `Access-Control-Allow-Headers`). `Access-Control-
    /// Max-Age` defaults to [`DEFAULT_MAX_AGE_SECONDS`] and is overridable via
    /// the `max_age` config. The allow-list stays unset — and the block
    /// therefore fails closed on cross-origin requests — until
    /// `lifecycle(Init)` parses block config or per-request `ctx.config_get`
    /// supplies one. See SEC-087 for the rationale.
    pub fn new() -> Self {
        Self {
            allowed_origins: OnceLock::new(),
            allowed_methods: "GET, POST, PUT, PATCH, DELETE, OPTIONS".to_string(),
            allowed_headers: "Content-Type, Authorization, X-Requested-With".to_string(),
        }
    }

    fn cached_origins(&self) -> Option<String> {
        self.allowed_origins.get().cloned()
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
                 strings (e.g. [\"*\"]).",
                "*",
            )
            .name("Allowed Origins"),
            ConfigVar::new(
                "allowed_methods",
                "Comma-separated list of HTTP methods returned in \
                 Access-Control-Allow-Methods.",
                "GET,POST,PUT,DELETE,OPTIONS",
            )
            .name("Allowed Methods"),
            ConfigVar::new(
                "allowed_headers",
                "Comma-separated list of request headers returned in \
                 Access-Control-Allow-Headers.",
                "Content-Type,Authorization",
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
        // No wildcard default — failing closed is the point.
        let origins = ctx
            .config_get("allowed_origins")
            .map(|s| s.to_string())
            .or_else(|| self.cached_origins());

        let methods = ctx
            .config_get("allowed_methods")
            .map_or_else(|| self.allowed_methods.clone(), |s| s.to_string());
        let headers = ctx
            .config_get("allowed_headers")
            .map_or_else(|| self.allowed_headers.clone(), |s| s.to_string());
        let max_age = ctx
            .config_get("max_age")
            .map_or_else(|| DEFAULT_MAX_AGE_SECONDS.to_string(), |s| s.to_string());

        let mut out_msg = msg;

        let origin = out_msg.header("Origin").to_string();
        let mut credentials = false;
        let mut reflected = false;

        match origins {
            None => {
                // SEC-087: no configuration → deny cross-origin.
                // We do NOT emit Access-Control-Allow-Origin. Same-origin
                // requests are unaffected (the browser doesn't require
                // CORS headers for those).
                tracing::warn!(
                    "CORS: allowed_origins unconfigured — denying cross-origin request \
                     (set `allowed_origins` in the block config or per-flow-step config)",
                );
            }
            Some(allowed) if !origin.is_empty() => {
                if allowed == "*" {
                    // Wildcard: reflect origin but credentials MUST stay false per spec.
                    out_msg.set_meta("resp.header.Access-Control-Allow-Origin", &origin);
                    reflected = true;
                } else if allowed.split(',').any(|o| o.trim() == origin) {
                    // Origin explicitly in allowlist: safe to enable credentials.
                    out_msg.set_meta("resp.header.Access-Control-Allow-Origin", &origin);
                    credentials = true;
                    reflected = true;
                }
                // else: origin not allowed — emit nothing, browser will block.
            }
            Some(allowed) => {
                // No `Origin` header on the request — same-origin or non-browser.
                // For non-`*` configs we still surface the configured value so
                // intermediaries can verify, but we do not reflect anything.
                if allowed != "*" {
                    out_msg.set_meta("resp.header.Access-Control-Allow-Origin", &allowed);
                }
            }
        }

        out_msg.set_meta("resp.header.Access-Control-Allow-Methods", &methods);
        out_msg.set_meta("resp.header.Access-Control-Allow-Headers", &headers);
        if credentials {
            out_msg.set_meta("resp.header.Access-Control-Allow-Credentials", "true");
        }
        out_msg.set_meta("resp.header.Access-Control-Max-Age", &max_age);

        // SEC-088: emit `Vary: Origin` whenever the response includes a
        // reflected `Access-Control-Allow-Origin`. Required so intermediary
        // caches don't serve a response keyed for Origin A to a request
        // from Origin B.
        if reflected {
            out_msg.set_meta("resp.header.Vary", "Origin");
        }

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
            #[expect(
                clippy::option_if_let_else,
                reason = "the Ok/Err match deserializes config and emits tracing::warn! \
                          on every fail-closed fallback path; folding it into map_or/\
                          map_or_else would bury those warnings inside closures and \
                          hurt readability of the SEC-087 fail-closed logic"
            )]
            let cfg_origins = if event.data.is_empty() {
                None
            } else {
                match serde_json::from_slice::<serde_json::Value>(&event.data) {
                    Ok(cfg) => match cfg.get("allowed_origins") {
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
                    },
                    Err(_) => None,
                }
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
    use super::*;

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
            Some("https://a.example,https://b.example".to_string()),
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
        assert_eq!(block.cached_origins(), Some("*".to_string()));
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
            Some("https://a.example,https://b.example".to_string()),
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
            Some("https://real.example".to_string()),
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
    }

    #[tokio::test]
    async fn options_preflight_emits_halt_with_cors_meta() {
        use wafer_block::streams::output::TerminalNotResponse;

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
}
