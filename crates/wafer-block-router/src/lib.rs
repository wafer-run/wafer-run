#![warn(missing_docs)]

//! `wafer-run/router` — config-driven request router block.
//!
//! Reads a `routes` JSON array from block config during `lifecycle(Init)` and
//! at handle-time matches each incoming [`Message`] against the configured
//! entries by action (or HTTP method, auto-normalized) and path pattern
//! (exact, `{var}` capture, or `/**` suffix wildcard). The first match wins
//! and the message is dispatched to the named handler block via
//! [`Context::call_block`]; captured path variables are stamped onto the
//! message as `req.param.*` meta. Misses return [`ErrorCode::NotFound`].
//!
//! Registered link-time as `wafer-run/router` via
//! [`wafer_block::register_static_block!`]; consumers don't construct
//! the block type directly.

use std::sync::OnceLock;

use wafer_block::*;

/// Normalizes an action token to the standard [`RequestAction`] vocabulary,
/// accepting either canonical action names (`"retrieve"`, `"create"`, …) or
/// HTTP methods via the shared wire-contract table in
/// [`wafer_block::http_codec`] (`GET`/`HEAD` → retrieve, `POST` → create,
/// `PUT`/`PATCH` → update, `DELETE` → delete, `OPTIONS` → execute). Tokens
/// that are not HTTP methods are passed through lowercased.
fn normalize_action(s: &str) -> String {
    http_codec::try_action_for_http_method(s).map_or_else(|| s.to_lowercase(), ToString::to_string)
}

/// A single route entry parsed from block config.
#[derive(Debug)]
pub struct Route {
    /// Path pattern for the route.
    pub path: String,
    /// Normalized action strings (HTTP methods mapped to action names).
    /// Used by the matcher.
    pub actions: Vec<String>,
    /// Raw action strings as the operator wrote them in config (no
    /// HTTP-method-to-action normalization). Used by diagnostic
    /// renderers like `seal()`-time error messages so the operator sees
    /// their original vocabulary.
    pub raw_actions: Vec<String>,
    /// Target block name to dispatch to.
    pub block: String,
}

/// A route table the router refuses to load, and why.
///
/// Returned by [`parse_routes`] and surfaced as an `InvalidArgument` Init
/// failure, so a misspelled or mistyped entry stops the runtime at start-up
/// instead of silently falling through to a broader route at request time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteConfigError {
    /// Index of the offending entry in `routes`, or `None` when the `routes`
    /// value itself is malformed.
    pub index: Option<usize>,
    /// What is wrong with it.
    pub reason: String,
}

impl std::fmt::Display for RouteConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.index {
            Some(i) => write!(f, "routes[{i}]: {}", self.reason),
            None => write!(f, "routes: {}", self.reason),
        }
    }
}

impl std::error::Error for RouteConfigError {}

/// Parse routes from block config. Accepts the raw JSON config value
/// (use `BlockConfig::as_value()` to extract from a `BlockConfig`).
///
/// An absent `routes` key is an empty table. Anything else must be an array
/// of objects, each with a non-empty string `path` and `block`, and at most
/// one of `actions` / `methods`, which must be an array of strings. The
/// first entry that is not is returned as a [`RouteConfigError`]; no entry
/// is ever dropped. Keys the router does not read (for example a `config`
/// a consumer keeps alongside the route) are ignored.
pub fn parse_routes(config: &serde_json::Value) -> Result<Vec<Route>, RouteConfigError> {
    let Some(routes) = config.get("routes") else {
        return Ok(Vec::new());
    };
    let Some(entries) = routes.as_array() else {
        return Err(RouteConfigError {
            index: None,
            reason: format!("expected a JSON array, got {routes}"),
        });
    };
    entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            parse_route(entry).map_err(|reason| RouteConfigError {
                index: Some(i),
                reason,
            })
        })
        .collect()
}

/// Parse one `routes` entry; the error is the reason it is malformed.
fn parse_route(entry: &serde_json::Value) -> Result<Route, String> {
    if !entry.is_object() {
        return Err(format!("expected an object, got {entry}"));
    }
    let required = |key: &str| match entry.get(key).and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err(format!("`{key}` must be a non-empty string in {entry}")),
    };
    let path = required("path")?;
    let block = required("block")?;
    let raw_actions = match (entry.get("actions"), entry.get("methods")) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "`actions` and `methods` are the same setting; give one, in {entry}"
            ))
        }
        (Some(list), None) | (None, Some(list)) => list
            .as_array()
            .and_then(|arr| {
                arr.iter()
                    .map(|v| v.as_str().map(String::from))
                    .collect::<Option<Vec<String>>>()
            })
            .ok_or_else(|| format!("`actions`/`methods` must be an array of strings in {entry}"))?,
        (None, None) => Vec::new(),
    };
    let actions = raw_actions.iter().map(|a| normalize_action(a)).collect();
    Ok(Route {
        path,
        actions,
        raw_actions,
        block,
    })
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
    /// Builds a fresh router with an empty, uninitialized route table; the
    /// table is populated once on `lifecycle(Init)` from block config.
    pub fn new() -> Self {
        Self {
            routes: OnceLock::new(),
        }
    }
}

#[wafer_async_trait]
impl Block for RouterBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/router",
            "0.0.1",
            "router@v1",
            "Config-driven router that dispatches to handler blocks",
        )
        .infrastructure()
        .flow_config(vec![ConfigVar::new(
            "routes",
            "JSON array of route entries; each entry is dispatched to a \
             handler block matched by method and path pattern.",
            "[]",
        )
        .name("Routes")])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let routes = self.routes.get().map_or(&[][..], Vec::as_slice);
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
            let mut routed_msg = msg;
            extract_path_vars(&route.path, &path, &mut routed_msg);

            // Dispatch to the matched handler block
            return ctx.call_block(&route.block, routed_msg, input).await;
        }

        // No route matched — 404
        OutputStream::error(WaferError {
            code: ErrorCode::NotFound,
            message: "no matching route".to_string(),
            meta: vec![],
        })
    }

    fn collect_block_refs(&self, config: &serde_json::Value) -> Vec<BlockConfigRef> {
        // A malformed table yields no references here; `lifecycle(Init)`
        // refuses the same table with the reason, so it is never served.
        parse_routes(config)
            .unwrap_or_default()
            .into_iter()
            .map(|route| BlockConfigRef {
                target: route.block,
                location: format!("route {}", route.path),
                detail: if route.raw_actions.is_empty() {
                    None
                } else {
                    Some(route.raw_actions.join(" "))
                },
            })
            .collect()
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init && self.routes.get().is_none() {
            let config = wafer_block::BlockConfig::from_event(&event);

            let routes = parse_routes(config.as_value()).map_err(|e| WaferError {
                code: ErrorCode::InvalidArgument,
                message: format!("wafer-run/router: {e}"),
                meta: vec![],
            })?;
            if routes.is_empty() {
                tracing::debug!("wafer-run/router initialized with no routes");
            }
            self.routes.set(routes).ok();
        }
        Ok(())
    }
}

wafer_block::register_static_block!("wafer-run/router", RouterBlock);

#[cfg(test)]
mod tests {
    #[test]
    fn route_carries_raw_and_normalized_actions() {
        use serde_json::json;
        let cfg = json!({
            "routes": [
                {"path": "/a", "block": "a-block", "methods": ["GET"]},
                {"path": "/b", "block": "b-block", "actions": ["retrieve"]},
                {"path": "/c", "block": "c-block"}
            ]
        });
        let routes = super::parse_routes(&cfg).expect("well-formed routes");
        assert_eq!(routes.len(), 3);

        // Route 0: methods=["GET"] -> raw=["GET"], actions=["retrieve"]
        assert_eq!(routes[0].raw_actions, vec!["GET".to_string()]);
        assert_eq!(routes[0].actions, vec!["retrieve".to_string()]);

        // Route 1: actions=["retrieve"] -> raw=["retrieve"], actions=["retrieve"] (already normalized)
        assert_eq!(routes[1].raw_actions, vec!["retrieve".to_string()]);
        assert_eq!(routes[1].actions, vec!["retrieve".to_string()]);

        // Route 2: no actions/methods -> both empty
        assert!(routes[2].raw_actions.is_empty());
        assert!(routes[2].actions.is_empty());
    }

    #[test]
    fn normalize_action_maps_methods_and_lowercases_other_tokens() {
        use serde_json::json;
        let cfg = json!({
            "routes": [
                {"path": "/m", "block": "m-block",
                 "methods": ["GET", "post", "Put", "PATCH", "DELETE", "OPTIONS"]},
                {"path": "/t", "block": "t-block",
                 "actions": ["retrieve", "LIST", "Custom-Op"]}
            ]
        });
        let routes = super::parse_routes(&cfg).expect("well-formed routes");
        // HTTP methods (any case) go through the shared http_codec table.
        assert_eq!(
            routes[0].actions,
            vec!["retrieve", "create", "update", "update", "delete", "execute"]
        );
        // Non-method tokens pass through lowercased — config accepts
        // canonical action names and custom vocabularies.
        assert_eq!(routes[1].actions, vec!["retrieve", "list", "custom-op"]);
    }

    #[test]
    fn router_block_collect_block_refs_smoke() {
        use serde_json::json;
        use wafer_block::Block;

        let block = super::RouterBlock::new();
        let cfg = json!({
            "routes": [
                {"path": "/a", "block": "org/a-block", "methods": ["GET"]},
                {"path": "/b", "block": "org/b-block", "actions": ["retrieve", "list"]},
                {"path": "/c", "block": "org/c-block"}
            ]
        });
        let refs = block.collect_block_refs(&cfg);
        assert_eq!(refs.len(), 3);

        assert_eq!(refs[0].target, "org/a-block");
        assert_eq!(refs[0].location, "route /a");
        assert_eq!(refs[0].detail.as_deref(), Some("GET"));

        assert_eq!(refs[1].target, "org/b-block");
        assert_eq!(refs[1].location, "route /b");
        assert_eq!(refs[1].detail.as_deref(), Some("retrieve list"));

        assert_eq!(refs[2].target, "org/c-block");
        assert_eq!(refs[2].location, "route /c");
        assert!(
            refs[2].detail.is_none(),
            "expected None detail when no actions/methods"
        );
    }

    #[test]
    fn parse_routes_refuses_every_malformed_entry() {
        use serde_json::json;
        for (bad, index) in [
            (json!({"routes": [{"path": "/x", "blok": "a"}]}), Some(0)),
            (
                json!({"routes": [{"path": "/ok", "block": "a"}, {"path": 42, "block": "b"}]}),
                Some(1),
            ),
            (json!({"routes": [{"block": "c"}]}), Some(0)),
            (json!({"routes": [{"path": "/n", "block": null}]}), Some(0)),
            (json!({"routes": [{"path": "", "block": "d"}]}), Some(0)),
            (
                json!({"routes": [{"path": "/m", "block": "e", "methods": "GET"}]}),
                Some(0),
            ),
            (
                json!({"routes": [{"path": "/m", "block": "e", "methods": ["GET", 1]}]}),
                Some(0),
            ),
            (
                json!({"routes": [{"path": "/m", "block": "e", "methods": ["GET"], "actions": ["create"]}]}),
                Some(0),
            ),
            (json!({"routes": ["/x"]}), Some(0)),
            (json!({"routes": "[]"}), None),
        ] {
            let err = super::parse_routes(&bad).expect_err(&format!("{bad} must be refused"));
            assert_eq!(err.index, index, "{bad}: {err}");
        }
    }

    #[test]
    fn parse_routes_ignores_keys_it_does_not_read() {
        use serde_json::json;
        let routes = super::parse_routes(&json!({
            "routes": [{"path": "/**", "block": "wafer-run/web", "config": {"web_spa": "true"}}]
        }))
        .expect("an extra key is not a malformed entry");
        assert_eq!(routes.len(), 1);
        assert!(super::parse_routes(&json!({}))
            .expect("no routes key")
            .is_empty());
    }

    /// The real Init path: a malformed table fails `lifecycle(Init)` instead
    /// of loading a router that silently lacks the entry.
    #[tokio::test]
    async fn init_fails_on_a_malformed_route() {
        use std::sync::Arc;

        use wafer_block::{streams::output::TerminalNotResponse, InputStream, Message};
        use wafer_test_support::builder::WaferBuilder;

        let wafer = WaferBuilder::new()
            .with_block("wafer-run/router", Arc::new(super::RouterBlock::new()))
            .with_config(
                "wafer-run/router",
                serde_json::json!({"routes": [{"path": "/x", "blok": "a"}]}),
            )
            .build()
            .await
            .expect("an Init failure is cached on the block, not fatal to start");
        let mut msg = Message::new("retrieve");
        msg.set_meta("req.action", "retrieve");
        msg.set_meta("req.resource", "/x");
        match wafer
            .run_block("wafer-run/router", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Error(e)) => {
                assert!(e.message.contains("routes[0]"), "{e:?}");
            }
            other => panic!("a route without `block` must fail Init, got {other:?}"),
        }
    }
}
