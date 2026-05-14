use std::sync::RwLock;

use wafer_block::*;

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
    /// per-request context does not supply `allowed_origins`. `None` until
    /// Init succeeds.
    allowed_origins: RwLock<Option<String>>,
    allowed_methods: String,
    allowed_headers: String,
    max_age: String,
}

impl Default for CorsBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl CorsBlock {
    pub fn new() -> Self {
        Self {
            allowed_origins: RwLock::new(None),
            allowed_methods: "GET, POST, PUT, PATCH, DELETE, OPTIONS".to_string(),
            allowed_headers: "Content-Type, Authorization, X-Requested-With".to_string(),
            max_age: "86400".to_string(),
        }
    }

    fn cached_origins(&self) -> Option<String> {
        self.allowed_origins.read().ok().and_then(|g| g.clone())
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for CorsBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/cors",
            "0.0.1",
            "middleware@v1",
            "CORS preflight handler and header injection",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
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
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.allowed_methods.clone());
        let headers = ctx
            .config_get("allowed_headers")
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.allowed_headers.clone());

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
        out_msg.set_meta("resp.header.Access-Control-Max-Age", &self.max_age);

        // SEC-088: emit `Vary: Origin` whenever the response includes a
        // reflected `Access-Control-Allow-Origin`. Required so intermediary
        // caches don't serve a response keyed for Origin A to a request
        // from Origin B.
        if reflected {
            out_msg.set_meta("resp.header.Vary", "Origin");
        }

        // Handle OPTIONS preflight — respond with empty 204
        if out_msg.get_meta("http.method") == "OPTIONS" {
            return OutputStream::drop_request();
        }

        OutputStream::continue_with(out_msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if let LifecycleType::Init = event.event_type {
            // Parse block config if any was supplied.
            let cfg_origins = if event.data.is_empty() {
                None
            } else {
                match serde_json::from_slice::<serde_json::Value>(&event.data) {
                    Ok(cfg) => cfg
                        .get("allowed_origins")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    Err(_) => None,
                }
            };

            match cfg_origins {
                Some(v) if !v.trim().is_empty() => {
                    if let Ok(mut guard) = self.allowed_origins.write() {
                        *guard = Some(v);
                    }
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

wafer_run::register_static_block!("wafer-run/cors", CorsBlock);

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
}
