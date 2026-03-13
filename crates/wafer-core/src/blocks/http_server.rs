//! `@wafer/http-server` — batteries-included HTTP server flow.
//!
//! Registers a flow that chains the standard infrastructure blocks
//! (security-headers, CORS, readonly-guard, rate-limiting, monitoring)
//! followed by the config-driven router. Pair with `@wafer/http-listener`
//! to get a fully working HTTP server from a single config:
//!
//! ```rust,ignore
//! wafer_core::blocks::http_server::register(&mut wafer);
//! wafer.add_block_config("@wafer/http-server", serde_json::json!({
//!     "listen": "0.0.0.0:8080",
//!     "routes": [{ "path": "/", "actions": ["retrieve"], "block": "hello" }]
//! }));
//! ```

use wafer_run::FlowDef;

const FLOW_JSON: &str = r#"{
    "id": "@wafer/http-server",
    "summary": "HTTP server: security headers, CORS, rate limiting, monitoring, router",
    "config": { "on_error": "stop" },
    "root": {
        "block": "@wafer/security-headers",
        "next": [{
            "block": "@wafer/cors",
            "next": [{
                "block": "@wafer/readonly-guard",
                "next": [{
                    "block": "@wafer/ip-rate-limit",
                    "next": [{
                        "block": "@wafer/monitoring",
                        "next": [{
                            "block": "@wafer/router"
                        }]
                    }]
                }]
            }]
        }]
    }
}"#;

/// Register the `@wafer/http-server` flow and all its block dependencies.
///
/// After calling this, configure with:
/// ```rust,ignore
/// wafer.add_block_config("@wafer/http-server", json!({
///     "listen": "0.0.0.0:8080",
///     "routes": [{ "path": "/hello", "block": "my-block" }]
/// }));
/// ```
pub fn register(w: &mut wafer_run::Wafer) {
    // Register blocks (idempotent — skips if already registered)
    if !w.has_block("@wafer/security-headers") {
        super::security_headers::register(w);
    }
    if !w.has_block("@wafer/cors") {
        super::cors::register(w);
    }
    if !w.has_block("@wafer/readonly-guard") {
        super::readonly_guard::register(w);
    }
    if !w.has_block("@wafer/ip-rate-limit") {
        super::ip_rate_limit::register(w);
    }
    if !w.has_block("@wafer/monitoring") {
        super::monitoring::register(w);
    }
    if !w.has_block("@wafer/router") {
        super::router::register(w);
    }
    if !w.has_block("@wafer/http-listener") {
        super::http_listener::register(w);
    }

    // Register flow
    let def: FlowDef = serde_json::from_str(FLOW_JSON)
        .expect("invalid @wafer/http-server flow JSON");
    w.add_flow_def(&def);

    // Config expander: split @wafer/http-server config into
    // @wafer/http-listener (listen + dispatch target) and @wafer/router (routes)
    w.add_config_expander("@wafer/http-server", |config| {
        let mut results = Vec::new();

        let listen = config
            .get("listen")
            .cloned()
            .unwrap_or(serde_json::json!("0.0.0.0:8080"));

        results.push((
            "@wafer/http-listener".to_string(),
            serde_json::json!({
                "flow": "@wafer/http-server",
                "listen": listen,
            }),
        ));

        if let Some(routes) = config.get("routes") {
            results.push((
                "@wafer/router".to_string(),
                serde_json::json!({ "routes": routes }),
            ));
        }

        results
    });
}
