//! BlockRegistry trait — abstracts block registration away from the concrete Wafer runtime.

use crate::block::Block;
use std::sync::Arc;

/// Trait for registering blocks without depending on the concrete `Wafer` runtime.
///
/// Block crates use this trait in their `register()` functions instead of
/// taking `&mut Wafer` directly, so they don't need to depend on `wafer-run`.
pub trait BlockRegistry {
    /// Register a block by name. Returns an error if the name is already registered.
    fn register_block(&mut self, name: &str, block: Arc<dyn Block>) -> Result<(), String>;

    /// Add a name alias (e.g. `"db"` → `"wafer-run/database"`).
    fn add_alias(&mut self, alias: &str, target: &str);

    /// Add JSON config for a block (merged during lifecycle Init).
    fn add_block_config(&mut self, name: &str, config: serde_json::Value);
}
