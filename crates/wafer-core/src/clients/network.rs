use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use wafer_run::common::ServiceOp;
use wafer_run::context::Context;
use wafer_run::types::WaferError;

use super::{call_service, decode};

const BLOCK: &str = "wafer/network";

// --- Wire-format types ---

#[derive(Serialize)]
struct DoReq<'a> {
    method: &'a str,
    url: &'a str,
    headers: &'a HashMap<String, String>,
    body: Option<&'a [u8]>,
}

/// Response from an outbound network request.
#[derive(Debug, Clone, Deserialize)]
pub struct NetworkResponse {
    pub status_code: u16,
    pub headers: HashMap<String, Vec<String>>,
    pub body: Vec<u8>,
}

// --- Public API ---

/// Perform an outbound HTTP request.
pub fn do_request(
    ctx: &dyn Context,
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> Result<NetworkResponse, WaferError> {
    let data = call_service(
        ctx,
        BLOCK,
        ServiceOp::NETWORK_DO_REQUEST,
        &DoReq {
            method,
            url,
            headers,
            body,
        },
    )?;
    decode(&data)
}
