use std::sync::{Arc, RwLock};
use wafer_block::*;

/// Access control policy for the inspector.
#[allow(dead_code)]
enum AccessPolicy {
    /// Require `auth.user_id` to be set (default).
    Authenticated,
    /// Allow unauthenticated access (dev mode).
    Anonymous,
    /// Require `auth.user_roles` to contain one of these roles.
    Roles(Vec<String>),
}

/// InspectorBlock provides runtime introspection — listing blocks, flows, and
/// serving a visual UI.
pub struct InspectorBlock {
    policy: RwLock<AccessPolicy>,
}

impl Default for InspectorBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl InspectorBlock {
    pub fn new() -> Self {
        Self {
            policy: RwLock::new(AccessPolicy::Roles(vec!["admin".to_string()])),
        }
    }
}

/// Build a JSON OutputStream response (bytes already serialized).
fn json_respond(json: Vec<u8>) -> OutputStream {
    OutputStream::respond_with_meta(
        json,
        vec![MetaEntry {
            key: META_RESP_CONTENT_TYPE.to_string(),
            value: "application/json".to_string(),
        }],
    )
}

/// Build an HTML OutputStream response.
fn html_respond(html: Vec<u8>) -> OutputStream {
    OutputStream::respond_with_meta(
        html,
        vec![MetaEntry {
            key: META_RESP_CONTENT_TYPE.to_string(),
            value: "text/html; charset=utf-8".to_string(),
        }],
    )
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for InspectorBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/inspector",
            "0.0.1",
            "http-handler@v1",
            "Runtime introspection — blocks, flows, and visual UI",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        // Access control
        {
            let policy = self.policy.read().unwrap();
            match &*policy {
                AccessPolicy::Anonymous => {}
                AccessPolicy::Authenticated => {
                    if msg.get_meta("auth.user_id").is_empty() {
                        return OutputStream::error(WaferError {
                            code: ErrorCode::Unauthenticated,
                            message: "inspector requires authentication".to_string(),
                            meta: vec![],
                        });
                    }
                }
                AccessPolicy::Roles(allowed) => {
                    if msg.get_meta("auth.user_id").is_empty() {
                        return OutputStream::error(WaferError {
                            code: ErrorCode::Unauthenticated,
                            message: "inspector requires authentication".to_string(),
                            meta: vec![],
                        });
                    }
                    let user_roles: Vec<&str> = msg
                        .get_meta("auth.user_roles")
                        .split(',')
                        .map(|r| r.trim())
                        .filter(|r| !r.is_empty())
                        .collect();
                    if !allowed.iter().any(|a| user_roles.contains(&a.as_str())) {
                        let roles_list = allowed.join(", ");
                        return OutputStream::error(WaferError {
                            code: ErrorCode::PermissionDenied,
                            message: format!(
                                "inspector requires one of these roles: [{}]",
                                roles_list
                            ),
                            meta: vec![],
                        });
                    }
                }
            }
        }

        // Only allow retrieve (GET)
        let action = msg.action().to_string();
        if !action.is_empty() && action != "retrieve" {
            return OutputStream::error(WaferError {
                code: ErrorCode::Unimplemented,
                message: "only retrieve action is allowed".to_string(),
                meta: vec![],
            });
        }

        let path = msg.path().to_string();

        // Suffix-based routing — works regardless of mount prefix
        if path.ends_with("/app") {
            let flows = ctx.flow_defs();
            let configs = ctx.block_configs();
            let blocks = ctx.registered_blocks();
            let interfaces = ctx.interface_specs();
            let json = serde_json::to_vec(&serde_json::json!({
                "flows": flows,
                "configs": configs,
                "blocks": blocks,
                "interfaces": interfaces,
            }))
            .unwrap_or_default();
            return json_respond(json);
        }

        if path.ends_with("/blocks") {
            let blocks = ctx.registered_blocks();
            let json = serde_json::to_vec(&blocks).unwrap_or_default();
            return json_respond(json);
        }

        if path.ends_with("/flows") {
            let flows = ctx.flow_infos();
            let json = serde_json::to_vec(&flows).unwrap_or_default();
            return json_respond(json);
        }

        if path.ends_with("/interfaces") {
            let interfaces = ctx.interface_specs();
            let json = serde_json::to_vec(&interfaces).unwrap_or_default();
            return json_respond(json);
        }

        if path.ends_with("/ui") {
            let html = include_str!("inspector.html");
            return html_respond(html.as_bytes().to_vec());
        }

        // /blocks/{name} — single block info
        if let Some(block_name) = extract_segment_after(&path, "/blocks/") {
            let decoded = url_decode(&block_name);
            let blocks = ctx.registered_blocks();
            if let Some(info) = blocks.into_iter().find(|b| b.name == decoded) {
                let json = serde_json::to_vec(&info).unwrap_or_default();
                return json_respond(json);
            }
            return OutputStream::error(WaferError {
                code: ErrorCode::NotFound,
                message: format!("block '{}' not found", decoded),
                meta: vec![],
            });
        }

        // /flows/{id} — single flow def
        if let Some(flow_id) = extract_segment_after(&path, "/flows/") {
            let decoded = url_decode(&flow_id);
            let defs = ctx.flow_defs();
            if let Some(def) = defs.into_iter().find(|c| c.id == decoded) {
                let json = serde_json::to_vec(&def).unwrap_or_default();
                return json_respond(json);
            }
            return OutputStream::error(WaferError {
                code: ErrorCode::NotFound,
                message: format!("flow '{}' not found", decoded),
                meta: vec![],
            });
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
        json_respond(serde_json::to_vec(&summary).unwrap_or_default())
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if let LifecycleType::Init = event.event_type {
            if let Ok(config) = serde_json::from_slice::<serde_json::Value>(&event.data) {
                // "allow_anonymous": true  → anyone can access
                if config
                    .get("allow_anonymous")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    *self.policy.write().unwrap() = AccessPolicy::Anonymous;
                    tracing::warn!(
                        "inspector: anonymous access enabled — do not use in production"
                    );
                }
                // "allowed_roles": ["admin", "developer"]  → only these roles
                else if let Some(roles) = config.get("allowed_roles").and_then(|v| v.as_array()) {
                    let role_list: Vec<String> = roles
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                    if !role_list.is_empty() {
                        tracing::info!("inspector: access restricted to roles: {:?}", role_list);
                        *self.policy.write().unwrap() = AccessPolicy::Roles(role_list);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Extract the segment value after a path prefix like "/blocks/".
/// e.g. "/foo/_inspector/blocks/my-block" with needle "/blocks/" -> Some("my-block")
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

/// Minimal percent-decoding for block/flow names (handles %2F -> /).
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

pub fn register(w: &mut dyn wafer_block::BlockRegistry) -> Result<(), String> {
    w.register_block("wafer-run/inspector", Arc::new(InspectorBlock::new()))
}
