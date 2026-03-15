//! `wafer-run/http-server` — batteries-included HTTP server flow.
//!
//! Registers a flow that chains the standard infrastructure blocks
//! (security-headers, CORS, readonly-guard, rate-limiting, monitoring)
//! followed by the config-driven router. A single call sets up a
//! fully working HTTP server:
//!
//! ```rust,ignore
//! wafer_flow_http_server::register(&mut wafer, serde_json::json!({
//!     "listen": "0.0.0.0:8080",
//!     "routes": [{ "path": "/**", "block": "hello" }]
//! }));
//! ```

use wafer_run::FlowDef;

const FLOW_JSON: &str = r#"{
    "id": "wafer-run/http-server",
    "summary": "HTTP server: security headers, CORS, rate limiting, monitoring, router",
    "config": { "on_error": "stop" },
    "blocks": [
        "wafer-run/security-headers",
        "wafer-run/cors",
        "wafer-run/readonly-guard",
        "wafer-run/ip-rate-limit",
        "wafer-run/monitoring",
        "wafer-run/router",
        "wafer-run/http-listener"
    ],
    "root": {
        "block": "wafer-run/security-headers",
        "next": [{
            "block": "wafer-run/cors",
            "next": [{
                "block": "wafer-run/readonly-guard",
                "next": [{
                    "block": "wafer-run/ip-rate-limit",
                    "next": [{
                        "block": "wafer-run/monitoring",
                        "next": [{
                            "block": "wafer-run/router"
                        }]
                    }]
                }]
            }]
        }]
    },
    "config_map": {
        "listen": { "target": "wafer-run/http-listener", "key": "listen" },
        "routes": { "target": "wafer-run/router", "key": "routes" }
    },
    "config_defaults": {
        "wafer-run/http-listener": { "flow": "wafer-run/http-server" }
    }
}"#;

/// Register the `wafer-run/http-server` flow with native blocks and config.
///
/// ```rust,ignore
/// wafer_flow_http_server::register(&mut wafer, json!({
///     "listen": "0.0.0.0:8080",
///     "routes": [{ "path": "/**", "block": "hello" }]
/// }));
/// ```
pub fn register(w: &mut wafer_run::Wafer, config: serde_json::Value) {
    // Register native blocks (idempotent — skips if already registered)
    if !w.has_block("wafer-run/security-headers") {
        wafer_block_security_headers::register(w);
    }
    if !w.has_block("wafer-run/cors") {
        wafer_block_cors::register(w);
    }
    if !w.has_block("wafer-run/readonly-guard") {
        wafer_block_readonly_guard::register(w);
    }
    if !w.has_block("wafer-run/ip-rate-limit") {
        wafer_block_ip_rate_limit::register(w);
    }
    if !w.has_block("wafer-run/monitoring") {
        wafer_block_monitoring::register(w);
    }
    if !w.has_block("wafer-run/router") {
        wafer_block_router::register(w);
    }
    if !w.has_block("wafer-run/http-listener") {
        wafer_block_http_listener::register(w);
    }

    // Register flow (config_map + config_defaults replace the old config expander)
    let def: FlowDef = serde_json::from_str(FLOW_JSON)
        .expect("invalid wafer-run/http-server flow JSON");
    w.add_flow_def(&def);

    // Set config
    w.add_block_config("wafer-run/http-server", config);
}
