use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::types::*;
use crate::wasm::capabilities::BlockCapabilities;

/// Block is the core interface every WAFER block must implement.
pub trait Block: Send + Sync {
    fn info(&self) -> BlockInfo;
    fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_;
    fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> std::result::Result<(), WaferError>;

    /// Called after the runtime is wrapped in Arc, giving blocks a clonable
    /// handle to execute flows. Only blocks that spawn async tasks (like the
    /// HTTP listener) need to override this.
    fn bind(&self, _handle: crate::runtime::RuntimeHandle) {}

    /// Return the capability restrictions for this block, if any.
    /// None means unrestricted (native blocks). WASM blocks return Some(&caps).
    fn block_capabilities(&self) -> Option<&BlockCapabilities> {
        None
    }
}

/// AdminUIInfo declares that a block provides an admin UI page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminUIInfo {
    pub path: String,
    pub icon: String,
    pub title: String,
}

/// BlockInfo declares a block's identity and capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockInfo {
    pub name: String,
    pub version: String,
    pub interface: String,
    pub summary: String,
    pub instance_mode: InstanceMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_modes: Vec<InstanceMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin_ui: Option<AdminUIInfo>,
}

impl BlockInfo {
    /// AllowsMode returns true if the block supports the given instance mode.
    pub fn allows_mode(&self, mode: InstanceMode) -> bool {
        if self.allowed_modes.is_empty() {
            return true;
        }
        self.allowed_modes.contains(&mode)
    }
}
