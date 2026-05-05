//! Typed client for the config service.
//!
//! Both ops are buffered single-frame request/response. `set` returns an
//! empty acknowledgement; `get` returns the current value as a
//! [`GetResponse`].

use wafer_block::{
    codec,
    wire::config::{GetRequest, GetResponse, SetRequest},
    ServiceOp, WaferError,
};

use super::common::{collect_single_frame, consume_ack, open_buffered};

const BLOCK: &str = "wafer-run/config";

/// Buffered: read a config key. The response carries the current value as a
/// string.
pub fn get(request: &GetRequest) -> Result<GetResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::CONFIG_GET, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "config GET")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding config GET response: {}", e.message),
        )
    })
}

/// Buffered: write a config key. The response is an empty acknowledgement.
pub fn set(request: &SetRequest) -> Result<(), WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(BLOCK, ServiceOp::CONFIG_SET, &req_bytes)?;
    consume_ack(&mut response_stream)
}
