use std::sync::Arc;
use wafer_run::block::{Block, BlockInfo};
use wafer_run::registry::BlockFactory;
use wafer_run::*;

/// A single route entry parsed from block config.
struct Route {
    path: String,
    methods: Vec<String>,
    block: String,
}

/// HttpRouterBlock matches incoming HTTP method/path against configured routes
/// and dispatches to the appropriate handler block via `ctx.call_block()`.
pub struct HttpRouterBlock {
    routes: Vec<Route>,
}

impl Block for HttpRouterBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/http-router".to_string(),
            version: "0.1.0".to_string(),
            interface: "router@v1".to_string(),
            summary: "Config-driven HTTP router that dispatches to handler blocks".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }

    fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let method = msg.get_meta("http.method").to_string();
        let path = msg.path().to_string();

        for route in &self.routes {
            // Check method match (empty methods list matches any method)
            if !route.methods.is_empty()
                && !route.methods.iter().any(|m| m.eq_ignore_ascii_case(&method))
            {
                continue;
            }

            // Check path match
            if !match_path(&route.path, &path) {
                continue;
            }

            // Extract path variables into req.param.* meta
            extract_path_vars(&route.path, &path, msg);

            // Dispatch to the matched handler block
            return ctx.call_block(&route.block, msg);
        }

        // No route matched — 404
        err_not_found(msg.clone(), "no matching route")
    }

    fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

/// Factory that parses the routes config array and creates an HttpRouterBlock.
pub struct HttpRouterBlockFactory;

impl BlockFactory for HttpRouterBlockFactory {
    fn create(&self, config: Option<&serde_json::Value>) -> Arc<dyn Block> {
        let routes: Vec<Route> = config
            .and_then(|c| c.get("routes"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| {
                        let path = entry.get("path")?.as_str()?.to_string();
                        let block = entry.get("block")?.as_str()?.to_string();
                        let methods = entry
                            .get("methods")
                            .and_then(|m| m.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_uppercase()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        Some(Route {
                            path,
                            methods,
                            block,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        if routes.is_empty() {
            tracing::warn!("@wafer/http-router created with no routes");
        }

        Arc::new(HttpRouterBlock { routes })
    }

    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/http-router".to_string(),
            version: "0.1.0".to_string(),
            interface: "router@v1".to_string(),
            summary: "Config-driven HTTP router that dispatches to handler blocks".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }
}

pub fn register(w: &mut Wafer) {
    w.registry()
        .register("@wafer/http-router", Arc::new(HttpRouterBlockFactory))
        .ok();
}
