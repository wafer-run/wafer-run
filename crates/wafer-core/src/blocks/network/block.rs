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

impl Block for NetworkBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer/network".to_string(),
            version: "0.1.0".to_string(),
            interface: "wafer.infra.network".to_string(),
            summary: "Outbound network requests via NetworkService".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }

    fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
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
                let request = Request {
                    method: req.method,
                    url: req.url,
                    headers: req.headers,
                    body: req.body,
                };
                match self.service.do_request(&request) {
                    Ok(resp) => respond_json(
                        msg,
                        &DoResponse {
                            status_code: resp.status_code,
                            headers: resp.headers,
                            body: resp.body,
                        },
                    ),
                    Err(e) => Result_::error(network_error_to_wafer(e)),
                }
            }
            other => Result_::error(WaferError::new(
                ErrorCode::UNIMPLEMENTED,
                format!("unknown network operation: {other}"),
            )),
        }
    }

    fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}
