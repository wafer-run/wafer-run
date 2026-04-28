use std::sync::RwLock;

use wafer_block::*;

const DEFAULT_CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self' https:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'";

/// SecurityHeadersBlock adds standard security headers to responses.
///
/// CSP is configurable via `block_config` — the runtime serializes the
/// config JSON to bytes and passes them in at `lifecycle(Init)`. Until
/// Init runs, the block uses the restrictive `DEFAULT_CSP`. Store via
/// `RwLock<String>` because `handle` takes `&self` and the config is
/// written once at Init, read on every request.
pub struct SecurityHeadersBlock {
    csp: RwLock<String>,
}

impl Default for SecurityHeadersBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl SecurityHeadersBlock {
    pub fn new() -> Self {
        Self {
            csp: RwLock::new(DEFAULT_CSP.to_string()),
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
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let csp = self
            .csp
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|_| DEFAULT_CSP.to_string());

        let mut out_msg = msg;
        out_msg.set_meta("resp.header.X-Content-Type-Options", "nosniff");
        out_msg.set_meta("resp.header.X-Frame-Options", "DENY");
        out_msg.set_meta("resp.header.X-XSS-Protection", "1; mode=block");
        out_msg.set_meta(
            "resp.header.Referrer-Policy",
            "strict-origin-when-cross-origin",
        );
        out_msg.set_meta("resp.header.Content-Security-Policy", &csp);
        out_msg.set_meta(
            "resp.header.Strict-Transport-Security",
            "max-age=31536000; includeSubDomains",
        );
        out_msg.set_meta(
            "resp.header.Permissions-Policy",
            "camera=(), microphone=(), geolocation=()",
        );

        OutputStream::continue_with(out_msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if let LifecycleType::Init = event.event_type {
            if let Ok(cfg) = serde_json::from_slice::<serde_json::Value>(&event.data) {
                if let Some(csp) = cfg.get("csp").and_then(|v| v.as_str()) {
                    if let Ok(mut guard) = self.csp.write() {
                        *guard = csp.to_string();
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
::wafer_run::inventory::submit! {
    ::wafer_run::StaticBlockRegistration {
        name: "wafer-run/security-headers",
        factory: || ::std::sync::Arc::new(SecurityHeadersBlock::new())
            as ::std::sync::Arc<dyn ::wafer_run::Block>,
    }
}
