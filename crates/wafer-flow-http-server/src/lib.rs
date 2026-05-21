#![warn(missing_docs)]
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

const FLOW_JSON: &str = r#"{
    "id": "wafer-run/http-server",
    "name": "HTTP Server",
    "version": "0.1.0",
    "description": "HTTP server: security headers, CORS, rate limiting, monitoring, router",
    "steps": [
        { "id": "security-headers", "block": "wafer-run/security-headers" },
        { "id": "cors", "block": "wafer-run/cors" },
        { "id": "readonly-guard", "block": "wafer-run/readonly-guard" },
        { "id": "rate-limit", "block": "wafer-run/ip-rate-limit" },
        { "id": "monitoring", "block": "wafer-run/monitoring" },
        { "id": "router", "block": "wafer-run/router" }
    ],
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
/// All blocks in the flow (security-headers, cors, readonly-guard, ip-rate-limit,
/// monitoring, router, http-listener) are inventory-managed via
/// `register_static_block!` and load automatically during `Wafer::new()`. This
/// function adds the flow definition and applies config.
///
/// ```rust,ignore
/// wafer_flow_http_server::register(&mut wafer, json!({
///     "listen": "0.0.0.0:8080",
///     "routes": [{ "path": "/**", "block": "hello" }]
/// }));
/// ```
pub fn register(
    w: &mut wafer_run::Wafer,
    config: serde_json::Value,
) -> Result<(), wafer_run::RuntimeError> {
    // Register flow
    w.add_flow_json(FLOW_JSON)?;

    // Set config
    w.add_block_config("wafer-run/http-server", config);
    Ok(())
}
