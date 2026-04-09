//! Message-based router for blocks.

use crate::common::ErrorCode;
use crate::context::Context;
use crate::executor::{extract_path_vars, match_path};
use crate::helpers;
use crate::meta::*;
use crate::{Message, RequestAction, Result_};

/// Handler function type for routes.
#[cfg(not(target_arch = "wasm32"))]
type RouteHandler = Box<dyn Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync>;
#[cfg(target_arch = "wasm32")]
type RouteHandler = Box<dyn Fn(&dyn Context, &mut Message) -> Result_>;

/// Route defines a route in a message-based router.
pub(crate) struct Route {
    action: String,
    pattern: String,
    handler: RouteHandler,
}

/// Router routes wafer messages based on request action + resource path.
pub struct Router {
    routes: Vec<Route>,
}

/// Generate a convenience method that delegates to `on()` with a fixed action.
macro_rules! route_method {
    ($name:ident, $action:expr) => {
        #[cfg(not(target_arch = "wasm32"))]
        pub fn $name(
            &mut self,
            pattern: impl Into<String>,
            handler: impl Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync + 'static,
        ) {
            self.on($action, pattern, handler);
        }
        #[cfg(target_arch = "wasm32")]
        pub fn $name(
            &mut self,
            pattern: impl Into<String>,
            handler: impl Fn(&dyn Context, &mut Message) -> Result_ + 'static,
        ) {
            self.on($action, pattern, handler);
        }
    };
}

impl Router {
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn on(
        &mut self,
        action: RequestAction,
        pattern: impl Into<String>,
        handler: impl Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync + 'static,
    ) {
        self.routes.push(Route {
            action: action.as_str().to_string(),
            pattern: pattern.into(),
            handler: Box::new(handler),
        });
    }

    #[cfg(target_arch = "wasm32")]
    pub fn on(
        &mut self,
        action: RequestAction,
        pattern: impl Into<String>,
        handler: impl Fn(&dyn Context, &mut Message) -> Result_ + 'static,
    ) {
        self.routes.push(Route {
            action: action.as_str().to_string(),
            pattern: pattern.into(),
            handler: Box::new(handler),
        });
    }

    route_method!(retrieve, RequestAction::Retrieve);
    route_method!(create, RequestAction::Create);
    route_method!(update, RequestAction::Update);
    route_method!(delete, RequestAction::Delete);
    route_method!(execute, RequestAction::Execute);

    pub fn route(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let action = msg.get_meta(META_REQ_ACTION).to_string();
        let path = msg.get_meta(META_REQ_RESOURCE).to_string();

        for route in &self.routes {
            if route.action != action {
                continue;
            }
            if !match_path(&route.pattern, &path) {
                continue;
            }
            extract_path_vars(&route.pattern, &path, msg);
            return (route.handler)(ctx, msg);
        }

        if action == RequestAction::Execute.as_str() {
            return msg.drop_msg_ref();
        }

        helpers::error(
            msg,
            ErrorCode::NOT_FOUND,
            &format!("route not found: {} {}", action, path),
        )
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}
