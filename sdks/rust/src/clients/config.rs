//! Typed client for the config service.
//!
//! Both ops are buffered single-frame request/response. `set` returns an
//! empty acknowledgement; `get` returns the current value as a
//! [`GetResponse`].

use wafer_block::{
    wire::config::{GetRequest, GetResponse, SetRequest},
    ServiceOp, WaferError,
};

use super::common::{call, call_ack};

const BLOCK: &str = "wafer-run/config";

/// Buffered: read a config key. The response carries the current value as a
/// string.
pub fn get(request: &GetRequest) -> Result<GetResponse, WaferError> {
    call(BLOCK, ServiceOp::CONFIG_GET, request)
}

/// Buffered: write a config key. The response is an empty acknowledgement.
pub fn set(request: &SetRequest) -> Result<(), WaferError> {
    call_ack(BLOCK, ServiceOp::CONFIG_SET, request)
}
