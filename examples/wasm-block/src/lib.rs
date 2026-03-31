//! Example WASM block: network-request-example
//!
//! Demonstrates how to build a WASM block that uses the network block to fetch
//! external URLs and process the results.
//!
//! Routes:
//!   GET /b/network-example/fetch?url=<url>  — fetch a URL, return metadata + body preview
//!   GET /b/network-example/json?url=<url>   — fetch a JSON API, extract and return the data
//!
//! Build:
//!   cargo build --target wasm32-wasip1 --release
//!
//! Requires:
//!   wafer-run/network block registered in the runtime

// Generate local WASM export stubs, remapping all WIT types to wafer_block's
// types so we get full SDK access (helpers, MessageExt, etc.).
wit_bindgen::generate!({
    world: "wafer-block",
    path: "../../wit/wit",
    with: {
        "wafer:block-world/types@0.2.0": wafer_block::wafer::block_world::types,
        "wafer:block-world/runtime@0.2.0": wafer_block::wafer::block_world::runtime,
    },
    export_macro_name: "export_block",
});

use wafer_block::*;
use wafer_core::clients::network;

use std::collections::HashMap;

struct NetworkExampleBlock;

#[wafer_block(
    name = "example/network-request",
    version = "0.1.0",
    interface = "http-handler@v1",
    summary = "Example: fetch URLs via the network block and process results",
    instance_mode = "singleton",
    requires = ["wafer-run/network"],
    export_macro = "export_block"
)]
impl NetworkExampleBlock {
    fn handle(msg: Message) -> BlockResult {
        let path = msg.get_meta("req.resource");
        let action = msg.get_meta("req.action");

        match (action, path) {
            ("retrieve", "/b/network-example/fetch") => handle_fetch(&msg),
            ("retrieve", "/b/network-example/json") => handle_json(&msg),
            _ => err_not_found(&msg, "not found"),
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Fetch any URL and return metadata about the response: status, headers,
/// content length, and a truncated body preview.
fn handle_fetch(msg: &Message) -> BlockResult {
    let url = msg.query("url");
    if url.is_empty() {
        return err_bad_request(msg, "Missing required query parameter: url");
    }

    let headers = HashMap::new();
    let resp = match network::do_request("GET", url, &headers, None) {
        Ok(r) => r,
        Err(e) => return err_internal(msg, &format!("Network request failed: {}", e.message)),
    };

    let body_len = resp.body.len();
    let body_preview = String::from_utf8_lossy(
        &resp.body[..body_len.min(512)]
    ).to_string();

    let content_type = resp.headers
        .get("content-type")
        .and_then(|v| v.first())
        .cloned()
        .unwrap_or_default();

    json_respond(msg, &serde_json::json!({
        "url": url,
        "status_code": resp.status_code,
        "content_type": content_type,
        "content_length": body_len,
        "body_preview": body_preview,
        "header_count": resp.headers.len(),
    }))
}

/// Fetch a JSON API and return the parsed data. Demonstrates deserializing
/// the network response and doing some processing before returning.
fn handle_json(msg: &Message) -> BlockResult {
    let url = msg.query("url");
    if url.is_empty() {
        return err_bad_request(msg, "Missing required query parameter: url");
    }

    let mut headers = HashMap::new();
    headers.insert("Accept".to_string(), "application/json".to_string());

    let resp = match network::do_request("GET", url, &headers, None) {
        Ok(r) => r,
        Err(e) => return err_internal(msg, &format!("Network request failed: {}", e.message)),
    };

    if resp.status_code >= 400 {
        return err_internal(msg, &format!(
            "Upstream returned HTTP {}", resp.status_code
        ));
    }

    let data: serde_json::Value = match serde_json::from_slice(&resp.body) {
        Ok(v) => v,
        Err(e) => return err_internal(msg, &format!("Failed to parse JSON response: {e}")),
    };

    json_respond(msg, &serde_json::json!({
        "url": url,
        "status_code": resp.status_code,
        "data": data,
        "keys": data.as_object().map(|o| o.keys().collect::<Vec<_>>()),
    }))
}
