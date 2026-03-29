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

    #[cfg(not(target_arch = "wasm32"))]
    pub fn retrieve(&mut self, pattern: impl Into<String>, handler: impl Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync + 'static) {
        self.on(RequestAction::Retrieve, pattern, handler);
    }
    #[cfg(target_arch = "wasm32")]
    pub fn retrieve(&mut self, pattern: impl Into<String>, handler: impl Fn(&dyn Context, &mut Message) -> Result_ + 'static) {
        self.on(RequestAction::Retrieve, pattern, handler);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn create(&mut self, pattern: impl Into<String>, handler: impl Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync + 'static) {
        self.on(RequestAction::Create, pattern, handler);
    }
    #[cfg(target_arch = "wasm32")]
    pub fn create(&mut self, pattern: impl Into<String>, handler: impl Fn(&dyn Context, &mut Message) -> Result_ + 'static) {
        self.on(RequestAction::Create, pattern, handler);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn update(&mut self, pattern: impl Into<String>, handler: impl Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync + 'static) {
        self.on(RequestAction::Update, pattern, handler);
    }
    #[cfg(target_arch = "wasm32")]
    pub fn update(&mut self, pattern: impl Into<String>, handler: impl Fn(&dyn Context, &mut Message) -> Result_ + 'static) {
        self.on(RequestAction::Update, pattern, handler);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn delete(&mut self, pattern: impl Into<String>, handler: impl Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync + 'static) {
        self.on(RequestAction::Delete, pattern, handler);
    }
    #[cfg(target_arch = "wasm32")]
    pub fn delete(&mut self, pattern: impl Into<String>, handler: impl Fn(&dyn Context, &mut Message) -> Result_ + 'static) {
        self.on(RequestAction::Delete, pattern, handler);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn execute(&mut self, pattern: impl Into<String>, handler: impl Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync + 'static) {
        self.on(RequestAction::Execute, pattern, handler);
    }
    #[cfg(target_arch = "wasm32")]
    pub fn execute(&mut self, pattern: impl Into<String>, handler: impl Fn(&dyn Context, &mut Message) -> Result_ + 'static) {
        self.on(RequestAction::Execute, pattern, handler);
    }

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
