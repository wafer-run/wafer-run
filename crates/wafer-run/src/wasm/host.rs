use std::sync::Arc;
use crate::context::Context;
use crate::wasm::capabilities::BlockCapabilities;

/// HostState stores the wafer Context and capabilities for host function calls.
pub struct HostState {
    pub context: Option<Arc<dyn Context>>,
    pub capabilities: BlockCapabilities,
}
