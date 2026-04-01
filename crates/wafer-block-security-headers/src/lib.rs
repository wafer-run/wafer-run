use std::sync::Arc;
use wafer_block::*;

/// SecurityHeadersBlock adds standard security headers to responses.
pub struct SecurityHeadersBlock {
    csp: String,
}

impl Default for SecurityHeadersBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl SecurityHeadersBlock {
    pub fn new() -> Self {
        Self {
            csp: "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self' https:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'".to_string(),
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for SecurityHeadersBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/security-headers",
            "0.0.1",
            "middleware@v1",
            "Adds standard security headers to HTTP responses",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Middleware)
    }

    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        // Read CSP from config if available
        let csp = ctx
            .config_get("csp")
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.csp.clone());

        msg.set_meta("resp.header.X-Content-Type-Options", "nosniff");
        msg.set_meta("resp.header.X-Frame-Options", "DENY");
        msg.set_meta("resp.header.X-XSS-Protection", "1; mode=block");
        msg.set_meta(
            "resp.header.Referrer-Policy",
            "strict-origin-when-cross-origin",
        );
        msg.set_meta("resp.header.Content-Security-Policy", &csp);
        msg.set_meta(
            "resp.header.Strict-Transport-Security",
            "max-age=31536000; includeSubDomains",
        );
        msg.set_meta(
            "resp.header.Permissions-Policy",
            "camera=(), microphone=(), geolocation=()",
        );

        msg.cont_ref()
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

pub fn register(w: &mut dyn wafer_block::BlockRegistry) {
    w.register_block(
        "wafer-run/security-headers",
        Arc::new(SecurityHeadersBlock::new()),
    );
}
