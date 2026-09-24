//! `WaferBuilder` — helper for assembling a test `Wafer` runtime.

use std::sync::Arc;

use wafer_block::Block;
use wafer_run::{RuntimeError, Wafer};

/// Fluent helper that assembles a minimal `Wafer` runtime from the blocks
/// and config a test registers. Disables inventory + lockfile loading so
/// each test starts from an empty registry.
pub struct WaferBuilder {
    wafer: Wafer,
}

impl Default for WaferBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl WaferBuilder {
    /// Create a builder wrapping a fresh `Wafer` with no blocks registered.
    pub fn new() -> Self {
        Self {
            wafer: Wafer::builder()
                .disable_inventory()
                .disable_lockfile()
                .build()
                .expect("empty wafer build is infallible"),
        }
    }

    /// Register an arbitrary block at `name`.
    pub fn with_block(mut self, name: &str, block: Arc<dyn Block>) -> Self {
        self.wafer
            .register_block(name, block)
            .expect("register block");
        self
    }

    /// Provide config for a registered block.
    pub fn with_config(mut self, block: &str, config: serde_json::Value) -> Self {
        self.wafer.add_block_config(block, config);
        self
    }

    /// Set the WRAP admin block — this block bypasses resource access checks.
    /// Use in tests where the block under test needs unrestricted DB/crypto access
    /// (e.g., infrastructure blocks that were written before WRAP naming conventions).
    pub fn with_admin_block(mut self, block_id: &str) -> Self {
        self.wafer.set_admin_block(block_id);
        self
    }

    /// Start the runtime. Returns `Arc<Wafer>`.
    pub async fn build(self) -> Result<Arc<Wafer>, RuntimeError> {
        self.wafer.start().await
    }
}
