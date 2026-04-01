use std::sync::{Arc, OnceLock};
use wafer_block::*;

/// Normalize a value to a standard action. Accepts both action names
/// (`"retrieve"`) and HTTP methods (`"GET"`).
fn normalize_action(s: &str) -> String {
    match s.to_uppercase().as_str() {
        "GET" | "HEAD" => "retrieve".to_string(),
        "POST" => "create".to_string(),
        "PUT" | "PATCH" => "update".to_string(),
        "DELETE" => "delete".to_string(),
        "OPTIONS" => "execute".to_string(),
        _ => s.to_lowercase(),
    }
}

/// A single route entry parsed from block config.
struct Route {
    path: String,
    actions: Vec<String>,
    block: String,
}

/// Parse routes from block config.
fn parse_routes(config: &wafer_block::BlockConfig) -> Vec<Route> {
    config
        .get("routes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    let path = entry.get("path")?.as_str()?.to_string();
                    let block = entry.get("block")?.as_str()?.to_string();
                    // Accept "actions" or "methods" — both are normalized
                    let raw = entry
                        .get("actions")
                        .or_else(|| entry.get("methods"))
                        .and_then(|m| m.as_array());
                    let actions = raw
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(normalize_action))
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(Route {
                        path,
                        actions,
                        block,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `wafer-run/router` matches incoming messages against configured routes
/// using standard message properties (`req.action`, `req.resource`) and
/// dispatches to the appropriate handler block via `ctx.call_block()`.
///
/// Transport-agnostic — works with any message that has standard meta.
///
/// Initialized during `lifecycle(Init)` from config (reads `routes` array).
///
/// Route paths support exact matches, `/**` wildcard suffixes, and `{var}`
/// path parameters:
/// ```json
/// { "path": "/users",       "block": "list-users" }
/// { "path": "/users/{id}",  "block": "get-user" }
/// { "path": "/static/**",   "block": "file-server" }
/// ```
///
/// Route config accepts either `"actions"` or `"methods"`:
/// ```json
/// { "path": "/users", "actions": ["retrieve"], "block": "list-users" }
/// { "path": "/users", "methods": ["GET"],      "block": "list-users" }
/// ```
/// HTTP methods are automatically mapped to actions (GET -> retrieve, etc.).
pub struct RouterBlock {
    routes: OnceLock<Vec<Route>>,
}

impl Default for RouterBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl RouterBlock {
    pub fn new() -> Self {
        Self {
            routes: OnceLock::new(),
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for RouterBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/router",
            "0.0.1",
            "router@v1",
            "Config-driven router that dispatches to handler blocks",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let routes = self.routes.get().map(|r| r.as_slice()).unwrap_or(&[]);
        let action = msg.action().to_string();
        let path = msg.path().to_string();

        for route in routes {
            // Check action match (empty list matches any action)
            if !route.actions.is_empty() && !route.actions.contains(&action) {
                continue;
            }

            // Check path match
            if !match_path(&route.path, &path) {
                continue;
            }

            // Extract path variables into req.param.* meta
            extract_path_vars(&route.path, &path, msg);

            // Dispatch to the matched handler block
            return ctx.call_block(&route.block, msg).await;
        }

        // No route matched — 404
        err_not_found(msg, "no matching route")
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init && self.routes.get().is_none() {
            let config = wafer_block::BlockConfig::from_event(&event);

            let routes = parse_routes(&config);
            if routes.is_empty() {
                tracing::debug!("wafer-run/router initialized with no routes");
            }
            self.routes.set(routes).ok();
        }
        Ok(())
    }
}

pub fn register(w: &mut dyn wafer_block::BlockRegistry) {
    w.register_block("wafer-run/router", Arc::new(RouterBlock::new()));
}
