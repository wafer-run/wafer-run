use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use crate::block::Block;

/// BlockFactory creates block instances by name.
pub type WaferBlockFactory = Arc<dyn Fn() -> Arc<dyn Block> + Send + Sync>;

/// BlockRegistry manages block factories.
pub struct BlockRegistry {
    factories: RwLock<HashMap<String, WaferBlockFactory>>,
}

impl BlockRegistry {
    pub fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    /// Register a block factory under the given name.
    pub fn register(&self, name: impl Into<String>, factory: WaferBlockFactory) {
        self.factories.write().insert(name.into(), factory);
    }

    /// Get a block factory by name.
    pub fn get(&self, name: &str) -> Option<WaferBlockFactory> {
        self.factories.read().get(name).cloned()
    }

    /// Create blocks from a list of block names.
    /// Returns (registered blocks, unresolved names).
    pub fn create_blocks(
        &self,
        names: &[String],
    ) -> Result<(HashMap<String, Arc<dyn Block>>, Vec<String>), String> {
        let mut registered = HashMap::new();
        let mut unresolved = Vec::new();

        for name in names {
            if let Some(factory) = self.get(name) {
                registered.insert(name.clone(), factory());
            } else {
                unresolved.push(name.clone());
            }
        }

        Ok((registered, unresolved))
    }
}

impl Default for BlockRegistry {
    fn default() -> Self {
        Self::new()
    }
}
