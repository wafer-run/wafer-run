#![warn(missing_docs)]
//! `wafer-run/http-server` — batteries-included HTTP server flow.
//!
//! Exports the flow definition and its registration key as constants so
//! callers wire it into a `Wafer` themselves:
//!
//! ```rust,ignore
//! wafer.add_flow_json(wafer_flow_http_server::FLOW_JSON)?;
//! wafer.add_block_config(
//!     wafer_flow_http_server::FLOW_ID,
//!     serde_json::json!({ "listen": "0.0.0.0:8080", "routes": [...] }),
//! );
//! ```
//!
//! All blocks in the flow (security-headers, cors, readonly-guard,
//! ip-rate-limit, monitoring, router, http-listener) self-register at
//! link time via `register_static_block!`. This crate carries them as
//! direct deps and force-links them via the `use … as _;` lines below,
//! so a binary that depends on `wafer-flow-http-server` gets every
//! block the flow needs without having to declare each one itself.

// Force-link every block referenced by FLOW_JSON. `register_static_block!`
// uses `linkme::distributed_slice` whose entries survive the linker only
// when the producer crate's object file is pulled into the binary. A bare
// `[dependencies]` entry isn't always enough — see the inventory tests in
// `wafer-run/tests/inventory_registration.rs` for the same pattern.
wafer_block::use_static_blocks!(
    cors,
    http_listener,
    ip_rate_limit,
    monitoring,
    readonly_guard,
    router,
    security_headers,
);

/// Flow id — the value `Wafer::add_block_config` keys composite config under.
pub const FLOW_ID: &str = "wafer-run/http-server";

/// Flow definition. Pair with [`FLOW_ID`] via `add_flow_json` +
/// `add_block_config`. See crate-level docs for the full pattern.
pub const FLOW_JSON: &str = r#"{
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
