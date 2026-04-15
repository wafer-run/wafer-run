//! Message-based router for blocks.

use crate::common::ErrorCode;
use crate::context::Context;
use crate::core_types::{Message, WaferError};
use crate::executor::{extract_path_vars, match_path};
use crate::meta::*;
use crate::streams::input::InputStream;
use crate::streams::output::OutputStream;
use crate::types::RequestAction;

/// Handler function type for routes.
#[cfg(not(target_arch = "wasm32"))]
type RouteHandler = Box<dyn Fn(&dyn Context, Message, InputStream) -> OutputStream + Send + Sync>;
#[cfg(target_arch = "wasm32")]
type RouteHandler = Box<dyn Fn(&dyn Context, Message, InputStream) -> OutputStream>;

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
            handler: impl Fn(&dyn Context, Message, InputStream) -> OutputStream
                + Send
                + Sync
                + 'static,
        ) {
            self.on($action, pattern, handler);
        }
        #[cfg(target_arch = "wasm32")]
        pub fn $name(
            &mut self,
            pattern: impl Into<String>,
            handler: impl Fn(&dyn Context, Message, InputStream) -> OutputStream + 'static,
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
        handler: impl Fn(&dyn Context, Message, InputStream) -> OutputStream + Send + Sync + 'static,
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
        handler: impl Fn(&dyn Context, Message, InputStream) -> OutputStream + 'static,
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

    pub fn route(&self, ctx: &dyn Context, mut msg: Message, input: InputStream) -> OutputStream {
        let action = msg.get_meta(META_REQ_ACTION).to_string();
        let path = msg.get_meta(META_REQ_RESOURCE).to_string();

        for route in &self.routes {
            if route.action != action {
                continue;
            }
            if !match_path(&route.pattern, &path) {
                continue;
            }
            extract_path_vars(&route.pattern, &path, &mut msg);
            return (route.handler)(ctx, msg, input);
        }

        if action == RequestAction::Execute.as_str() {
            return OutputStream::drop_request();
        }

        OutputStream::error(WaferError {
            code: ErrorCode::NotFound,
            message: format!("route not found: {} {}", action, path),
            meta: vec![],
        })
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}
