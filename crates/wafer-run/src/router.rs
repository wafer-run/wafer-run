use crate::context::Context;
use crate::executor::{extract_path_vars, match_path};
use crate::helpers;
use crate::meta::*;
use crate::types::*;

/// Route defines a route in a message-based router.
pub struct Route {
    pub action: String,
    pub pattern: String,
    pub handler: Box<dyn Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync>,
}

/// Router routes wafer messages based on request action + resource path.
pub struct Router {
    routes: Vec<Route>,
}

impl Router {
    /// Create an empty message router.
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    /// On registers a route for the given action and path pattern.
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

    /// Retrieve registers a route for retrieve (GET) requests.
    pub fn retrieve(
        &mut self,
        pattern: impl Into<String>,
        handler: impl Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync + 'static,
    ) {
        self.on(RequestAction::Retrieve, pattern, handler);
    }

    /// Create registers a route for create (POST) requests.
    pub fn create(
        &mut self,
        pattern: impl Into<String>,
        handler: impl Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync + 'static,
    ) {
        self.on(RequestAction::Create, pattern, handler);
    }

    /// Update registers a route for update (PUT/PATCH) requests.
    pub fn update(
        &mut self,
        pattern: impl Into<String>,
        handler: impl Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync + 'static,
    ) {
        self.on(RequestAction::Update, pattern, handler);
    }

    /// Delete registers a route for delete (DELETE) requests.
    pub fn delete(
        &mut self,
        pattern: impl Into<String>,
        handler: impl Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync + 'static,
    ) {
        self.on(RequestAction::Delete, pattern, handler);
    }

    /// Route finds the matching route, extracts path variables, and calls the handler.
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

        // OPTIONS/Execute handling for CORS preflight
        if msg.action() == RequestAction::Execute.as_str() {
            return helpers::respond(msg.clone(), 204, Vec::new(), "");
        }

        helpers::error(
            msg.clone(),
            404,
            "not_found",
            &format!("route not found: {} {}", action, path),
        )
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}
