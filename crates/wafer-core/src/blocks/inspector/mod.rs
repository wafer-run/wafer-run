use std::sync::Arc;
use wafer_run::*;

/// InspectorBlock provides runtime introspection — listing blocks, flows, and
/// serving a visual UI.
pub struct InspectorBlock;

impl InspectorBlock {
    pub fn new() -> Self {
        Self
    }
}

impl Block for InspectorBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/inspector".to_string(),
            version: "0.1.0".to_string(),
            interface: "handler@v1".to_string(),
            summary: "Runtime introspection — blocks, flows, and visual UI".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Wasm,
            requires: Vec::new(),
        }
    }

    fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        // Require authentication — reject if no auth.user_id is set
        let auth_user = msg.get_meta("auth.user_id");
        if auth_user.is_empty() {
            return err_unauthorized(msg, "inspector requires authentication");
        }

        // Only allow retrieve (GET)
        let action = msg.action();
        if !action.is_empty() && action != "retrieve" {
            return error(msg, "unimplemented", "only retrieve action is allowed");
        }

        let path = msg.path().to_string();

        // Suffix-based routing — works regardless of mount prefix
        if path.ends_with("/blocks") {
            let blocks = ctx.registered_blocks();
            return json_respond(msg, &blocks);
        }

        if path.ends_with("/flows") {
            let flows = ctx.flow_infos();
            return json_respond(msg, &flows);
        }

        if path.ends_with("/ui") {
            let html = include_str!("inspector.html");
            return respond(msg, html.as_bytes().to_vec(), "text/html; charset=utf-8");
        }

        // /blocks/{name} — single block info
        if let Some(block_name) = extract_segment_after(&path, "/blocks/") {
            let decoded = url_decode(&block_name);
            let blocks = ctx.registered_blocks();
            if let Some(info) = blocks.into_iter().find(|b| b.name == decoded) {
                return json_respond(msg, &info);
            }
            return err_not_found(msg, &format!("block '{}' not found", decoded));
        }

        // /flows/{id} — single flow def
        if let Some(flow_id) = extract_segment_after(&path, "/flows/") {
            let decoded = url_decode(&flow_id);
            let defs = ctx.flow_defs();
            if let Some(def) = defs.into_iter().find(|c| c.id == decoded) {
                return json_respond(msg, &def);
            }
            return err_not_found(msg, &format!("flow '{}' not found", decoded));
        }

        // Fallback: summary
        let blocks = ctx.registered_blocks();
        let flows = ctx.flow_infos();
        let summary = serde_json::json!({
            "block_count": blocks.len(),
            "flow_count": flows.len(),
            "blocks": blocks.iter().map(|b| &b.name).collect::<Vec<_>>(),
            "flows": flows.iter().map(|c| &c.id).collect::<Vec<_>>(),
        });
        json_respond(msg, &summary)
    }

    fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

/// Extract the segment value after a path prefix like "/blocks/".
/// e.g. "/foo/_inspector/blocks/my-block" with needle "/blocks/" → Some("my-block")
fn extract_segment_after(path: &str, needle: &str) -> Option<String> {
    let idx = path.find(needle)?;
    let rest = &path[idx + needle.len()..];
    if rest.is_empty() {
        return None;
    }
    // Take everything up to the next slash (or end)
    let segment = match rest.find('/') {
        Some(i) => &rest[..i],
        None => rest,
    };
    if segment.is_empty() {
        return None;
    }
    Some(segment.to_string())
}

/// Minimal percent-decoding for block/flow names (handles %2F → /).
/// Correctly handles multi-byte UTF-8 sequences.
fn url_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().unwrap_or(b'0');
            let lo = chars.next().unwrap_or(b'0');
            bytes.push(hex_val(hi) * 16 + hex_val(lo));
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| s.to_string())
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

pub fn register(w: &mut Wafer) {
    w.register_block("@wafer/inspector", Arc::new(InspectorBlock::new()));
}
