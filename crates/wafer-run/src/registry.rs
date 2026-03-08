use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use crate::block::{Block, BlockInfo};
use crate::context::Context;
use crate::types::*;

/// BlockFactory creates block instances.
pub trait BlockFactory: Send + Sync {
    /// Create returns a new block instance, initialized with the given config.
    fn create(&self, config: Option<&serde_json::Value>) -> Arc<dyn Block>;

    /// Info returns the block type's metadata.
    fn info(&self) -> BlockInfo;
}

/// StructBlockFactory creates blocks by cloning a prototype.
pub struct StructBlockFactory<F>
where
    F: Fn() -> Arc<dyn Block> + Send + Sync,
{
    pub new_func: F,
}

impl<F> BlockFactory for StructBlockFactory<F>
where
    F: Fn() -> Arc<dyn Block> + Send + Sync,
{
    fn create(&self, _config: Option<&serde_json::Value>) -> Arc<dyn Block> {
        (self.new_func)()
    }

    fn info(&self) -> BlockInfo {
        (self.new_func)().info()
    }
}

/// FuncBlock wraps a handler function as a Block.
pub struct FuncBlock {
    pub info: BlockInfo,
    pub handler: Box<dyn Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync>,
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for FuncBlock {
    fn info(&self) -> BlockInfo {
        self.info.clone()
    }

    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        (self.handler)(ctx, msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

/// Registry is a thread-safe catalog of block factories.
pub struct Registry {
    factories: RwLock<HashMap<String, Arc<dyn BlockFactory>>>,
}

impl Registry {
    /// Create a new empty Registry.
    pub fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    /// Register adds a block factory under the given type name.
    pub fn register(
        &self,
        type_name: impl Into<String>,
        factory: Arc<dyn BlockFactory>,
    ) -> std::result::Result<(), String> {
        let type_name = type_name.into();
        let mut factories = self.factories.write();
        if factories.contains_key(&type_name) {
            return Err(format!("block type {:?} already registered", type_name));
        }
        factories.insert(type_name, factory);
        Ok(())
    }

    /// RegisterFunc registers an inline block function.
    pub fn register_func(
        &self,
        type_name: impl Into<String>,
        handler: impl Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync + 'static,
    ) -> std::result::Result<(), String> {
        let type_name_str: String = type_name.into();
        let tn = type_name_str.clone();
        let handler = Arc::new(handler);
        let handler_clone = handler.clone();

        struct FuncFactory {
            type_name: String,
            handler: Arc<dyn Fn(&dyn Context, &mut Message) -> Result_ + Send + Sync>,
        }

        impl BlockFactory for FuncFactory {
            fn create(&self, _config: Option<&serde_json::Value>) -> Arc<dyn Block> {
                Arc::new(FuncBlock {
                    info: BlockInfo {
                        name: self.type_name.clone(),
                        version: "0.0.0".to_string(),
                        interface: "inline".to_string(),
                        summary: "Inline function block".to_string(),
                        instance_mode: InstanceMode::PerNode,
                        allowed_modes: Vec::new(),
                        admin_ui: None,
                        runtime: BlockRuntime::default(),
                        requires: Vec::new(),
                    },
                    handler: {
                        let h = self.handler.clone();
                        Box::new(move |ctx: &dyn Context, msg: &mut Message| h(ctx, msg))
                    },
                })
            }

            fn info(&self) -> BlockInfo {
                BlockInfo {
                    name: self.type_name.clone(),
                    version: "0.0.0".to_string(),
                    interface: "inline".to_string(),
                    summary: "Inline function block".to_string(),
                    instance_mode: InstanceMode::PerNode,
                    allowed_modes: Vec::new(),
                    admin_ui: None,
                    runtime: BlockRuntime::default(),
                    requires: Vec::new(),
                }
            }
        }

        self.register(
            type_name_str,
            Arc::new(FuncFactory {
                type_name: tn,
                handler: handler_clone,
            }),
        )
    }

    /// Get returns the factory for the given type name.
    pub fn get(&self, type_name: &str) -> Option<Arc<dyn BlockFactory>> {
        self.factories.read().get(type_name).cloned()
    }

    /// List returns info about all registered block types.
    pub fn list(&self) -> Vec<BlockInfo> {
        self.factories
            .read()
            .values()
            .map(|f| f.info())
            .collect()
    }

    /// Has returns true if a block type is registered.
    pub fn has(&self, type_name: &str) -> bool {
        self.factories.read().contains_key(type_name)
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}
