use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use wafer_run::block::{Block, BlockInfo};
use wafer_run::common::{ErrorCode, ServiceOp};
use wafer_run::context::Context;
use super::service::{NetworkError, NetworkService, Request};
use wafer_run::types::*;
use wafer_run::helpers::respond_json;

/// NetworkBlock wraps a NetworkService and exposes it as a Block.
pub struct NetworkBlock {
    service: Arc<dyn NetworkService>,
}

impl NetworkBlock {
    pub fn new(service: Arc<dyn NetworkService>) -> Self {
        Self { service }
    }
}

// --- Request types ---

#[derive(Deserialize)]
struct DoRequest {
    method: String,
    url: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    body: Option<Vec<u8>>,
}

// --- Response types ---

#[derive(Serialize)]
struct DoResponse {
    status_code: u16,
    headers: HashMap<String, Vec<String>>,
    body: Vec<u8>,
}

// --- Helpers ---

fn network_error_to_wafer(e: NetworkError) -> WaferError {
    match e {
        NetworkError::RequestError(msg) => WaferError::new(ErrorCode::UNAVAILABLE, msg),
        NetworkError::Other(msg) => WaferError::new(ErrorCode::INTERNAL, msg),
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for NetworkBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer-run/network".to_string(),
            version: "0.1.0".to_string(),
            interface: "network@v1".to_string(),
            summary: "Outbound network requests via NetworkService".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Native,
            requires: Vec::new(),
        }
    }

    async fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        match msg.kind.as_str() {
            ServiceOp::NETWORK_DO_REQUEST => {
                let req: DoRequest = match msg.decode() {
                    Ok(r) => r,
                    Err(e) => {
                        return Result_::error(WaferError::new(
                            ErrorCode::INVALID_ARGUMENT,
                            format!("invalid network.do request: {e}"),
                        ))
                    }
                };
                // SSRF protection: block requests to private/internal IPs.
                // Disabled when ALLOW_PRIVATE_NETWORK=true (for local dev/testing
                // with mock services like Stripe test mode).
                let allow_private = std::env::var("ALLOW_PRIVATE_NETWORK")
                    .map(|v| v == "true" || v == "1")
                    .unwrap_or(false);
                if !allow_private && wafer_run::security::is_blocked_url(&req.url) {
                    return Result_::error(WaferError::new(
                        ErrorCode::PERMISSION_DENIED,
                        "request to private/internal address is not allowed",
                    ));
                }

                let request = Request {
                    method: req.method,
                    url: req.url,
                    headers: req.headers,
                    body: req.body,
                };
                // Run blocking network I/O on a dedicated thread to avoid
                // panicking reqwest::blocking inside the tokio async runtime.
                let svc = self.service.clone();
                let result = tokio::task::spawn_blocking(move || {
                    svc.do_request(&request)
                }).await;
                match result {
                    Ok(Ok(resp)) => respond_json(
                        msg,
                        &DoResponse {
                            status_code: resp.status_code,
                            headers: resp.headers,
                            body: resp.body,
                        },
                    ),
                    Ok(Err(e)) => Result_::error(network_error_to_wafer(e)),
                    Err(e) => Result_::error(WaferError::new(
                        ErrorCode::INTERNAL,
                        format!("network task panicked: {e}"),
                    )),
                }
            }
            other => Result_::error(WaferError::new(
                ErrorCode::UNIMPLEMENTED,
                format!("unknown network operation: {other}"),
            )),
        }
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}
