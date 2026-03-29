//! Shared message handler logic for the network block.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use wafer_block::common::{ErrorCode, ServiceOp};
use wafer_block::helpers::respond_json;
use wafer_block::*;

use super::service::{NetworkError, NetworkService, Request};

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

/// Handle a network message by delegating to the given service.
///
/// SSRF protection is NOT included here — it is platform-specific.
/// Native callers should check `wafer_run::security::is_blocked_url` before
/// calling the service. CF Workers are sandboxed by the runtime.
pub async fn handle_message(service: &dyn NetworkService, msg: &mut Message) -> Result_ {
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

            match service.do_request(&request).await {
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
