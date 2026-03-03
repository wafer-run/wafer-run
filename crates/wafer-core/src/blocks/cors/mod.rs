use std::sync::Arc;
use wafer_run::*;

/// CorsBlock handles CORS preflight and sets CORS headers.
pub struct CorsBlock {
    allowed_origins: String,
    allowed_methods: String,
    allowed_headers: String,
    max_age: String,
}

impl CorsBlock {
    pub fn new() -> Self {
        Self {
            allowed_origins: "*".to_string(),
            allowed_methods: "GET, POST, PUT, PATCH, DELETE, OPTIONS".to_string(),
            allowed_headers: "Content-Type, Authorization, X-Requested-With".to_string(),
            max_age: "86400".to_string(),
        }
    }
}

impl Block for CorsBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/cors".to_string(),
            version: "0.1.0".to_string(),
            interface: "middleware@v1".to_string(),
            summary: "CORS preflight handler and header injection".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }

    fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let origins = ctx
            .config_get("allowed_origins")
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.allowed_origins.clone());
        let methods = ctx
            .config_get("allowed_methods")
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.allowed_methods.clone());
        let headers = ctx
            .config_get("allowed_headers")
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.allowed_headers.clone());

        // Set CORS headers on the message meta (bridge will apply them)
        let origin = msg.header("Origin").to_string();
        let mut credentials = false;
        if !origin.is_empty() {
            if origins == "*" {
                // Wildcard: reflect origin but credentials MUST stay false per spec
                msg.set_meta("resp.header.Access-Control-Allow-Origin", &origin);
            } else if origins.split(',').any(|o| o.trim() == origin) {
                // Origin explicitly in allowlist: safe to enable credentials
                msg.set_meta("resp.header.Access-Control-Allow-Origin", &origin);
                credentials = true;
            }
        } else {
            msg.set_meta("resp.header.Access-Control-Allow-Origin", &origins);
        }

        msg.set_meta("resp.header.Access-Control-Allow-Methods", &methods);
        msg.set_meta("resp.header.Access-Control-Allow-Headers", &headers);
        if credentials {
            msg.set_meta("resp.header.Access-Control-Allow-Credentials", "true");
        }
        msg.set_meta("resp.header.Access-Control-Max-Age", &self.max_age);

        // Handle OPTIONS preflight
        if msg.get_meta("http.method") == "OPTIONS" {
            return respond(msg.clone(), 204, Vec::new(), "");
        }

        msg.clone().cont()
    }

    fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

pub fn register(w: &mut Wafer) {
    w.register_block("@wafer/cors", Arc::new(CorsBlock::new()));
}
