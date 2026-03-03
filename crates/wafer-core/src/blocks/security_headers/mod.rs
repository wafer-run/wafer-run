use std::sync::Arc;
use wafer_run::*;

/// SecurityHeadersBlock adds standard security headers to responses.
pub struct SecurityHeadersBlock {
    csp: String,
}

impl SecurityHeadersBlock {
    pub fn new() -> Self {
        Self {
            csp: "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; img-src 'self' data: blob:; font-src 'self' https://fonts.gstatic.com; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'".to_string(),
        }
    }
}

impl Block for SecurityHeadersBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/security-headers".to_string(),
            version: "0.1.0".to_string(),
            interface: "middleware@v1".to_string(),
            summary: "Adds standard security headers to HTTP responses".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }

    fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        // Read CSP from config if available
        let csp = ctx
            .config_get("csp")
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.csp.clone());

        msg.set_meta("resp.header.X-Content-Type-Options", "nosniff");
        msg.set_meta("resp.header.X-Frame-Options", "DENY");
        msg.set_meta("resp.header.X-XSS-Protection", "1; mode=block");
        msg.set_meta("resp.header.Referrer-Policy", "strict-origin-when-cross-origin");
        msg.set_meta("resp.header.Content-Security-Policy", &csp);
        msg.set_meta(
            "resp.header.Strict-Transport-Security",
            "max-age=31536000; includeSubDomains",
        );
        msg.set_meta(
            "resp.header.Permissions-Policy",
            "camera=(), microphone=(), geolocation=()",
        );

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
    w.register_block("@wafer/security-headers", Arc::new(SecurityHeadersBlock::new()));
}
