use std::sync::Arc;
use wasmtime::StoreLimits;
use crate::context::Context;
use crate::wasm::capabilities::BlockCapabilities;

/// HostState stores the wafer Context, capabilities, and resource limits for host function calls.
pub struct HostState {
    pub context: Option<Arc<dyn Context>>,
    pub capabilities: BlockCapabilities,
    pub limiter: StoreLimits,
}
