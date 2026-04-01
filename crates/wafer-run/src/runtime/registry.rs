use std::sync::Arc;

use crate::block::{Block, FuncBlock};
use crate::types::*;

use super::Wafer;

impl Wafer {
    /// Add a named registrar function. Registrars are called by
    /// [`register`](Self::register) to set up blocks, flows, and config
    /// by name.
    ///
    /// Typically called by crate consumers (e.g. wafer-core) to make
    /// their blocks available via `wafer.register("wafer-run/...", config)`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn add_registrar(
        &mut self,
        name: impl Into<String>,
        f: impl Fn(&mut Wafer, serde_json::Value) + Send + Sync + 'static,
    ) {
        self.registrars.insert(name.into(), Box::new(f));
    }

    #[cfg(target_arch = "wasm32")]
    pub fn add_registrar(
        &mut self,
        name: impl Into<String>,
        f: impl Fn(&mut Wafer, serde_json::Value) + 'static,
    ) {
        self.registrars.insert(name.into(), Box::new(f));
    }

    /// Register a block or flow by name with the given config.
    ///
    /// If a registrar was previously added via [`add_registrar`](Self::add_registrar),
    /// it is called immediately. Otherwise, for names matching the
    /// `{org}/{block}` convention, the config is stored and the
    /// block or flow will be resolved during [`resolve()`](Self::resolve)
    /// (downloading `.flow.json` or `.wasm` via the registry).
    pub fn register(&mut self, name: &str, config: serde_json::Value) {
        if let Some(registrar) = self.registrars.remove(name) {
            registrar(self, config);
            self.registrars.insert(name.to_string(), registrar);
            return;
        }

        // No registrar — store config for deferred resolution during resolve().
        // The name must look like a remote ref (org/block).
        if !name.contains('/') {
            panic!("no registrar found for {:?} and name is not a remote ref", name);
        }
        tracing::debug!(name = %name, "no registrar found, deferring to resolve()");
        self.add_block_config(name, config);
    }

    /// Load block configurations from a JSON file.
    ///
    /// The file should be a JSON object mapping block names to config objects.
    /// Environment variables in `${VAR}` format are expanded before parsing.
    ///
    /// Native-only: requires filesystem access.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_blocks_json(&mut self, path: &str) -> Result<(), String> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| format!("read blocks.json {}: {}", path, e))?;

        let expanded = crate::helpers::expand_env_vars(&data);

        let mut map: std::collections::HashMap<String, serde_json::Value> = serde_json::from_str(&expanded)
            .map_err(|e| format!("parse blocks.json: {}", e))?;

        // Extract alias definitions before processing block configs
        if let Some(aliases_val) = map.remove("aliases") {
            if let Some(aliases_obj) = aliases_val.as_object() {
                for (alias, target) in aliases_obj {
                    if let Some(target_str) = target.as_str() {
                        self.aliases.insert(alias.clone(), target_str.to_string());
                    }
                }
            }
        }

        for (name, config) in map {
            self.block_configs.insert(name, config);
        }

        Ok(())
    }

    /// Add a block configuration programmatically.
    pub fn add_block_config(&mut self, name: impl Into<String>, config: serde_json::Value) {
        self.block_configs.insert(name.into(), config);
    }

    /// Register a config expander that splits a composite config into
    /// individual block configs. Called during `resolve()` before configs
    /// are distributed to blocks.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn add_config_expander(
        &mut self,
        name: impl Into<String>,
        expander: impl Fn(serde_json::Value) -> Vec<(String, serde_json::Value)> + Send + Sync + 'static,
    ) {
        self.config_expanders.insert(name.into(), Box::new(expander));
    }

    #[cfg(target_arch = "wasm32")]
    pub fn add_config_expander(
        &mut self,
        name: impl Into<String>,
        expander: impl Fn(serde_json::Value) -> Vec<(String, serde_json::Value)> + 'static,
    ) {
        self.config_expanders.insert(name.into(), Box::new(expander));
    }

    /// HasBlock returns true if a block with the given type name is registered.
    pub fn has_block(&self, type_name: &str) -> bool {
        self.blocks.contains_key(type_name)
    }

    /// RegisterBlock registers a block instance under the given type name.
    /// The instance is also pre-resolved so it is available via `call_block()`
    /// even when it is not referenced as a flow node.
    ///
    /// The block's `lifecycle(Init)` will be called during `start()` with
    /// config data from `add_block_config()` (if any) or empty data.
    pub fn register_block(&mut self, type_name: impl Into<String>, block: Arc<dyn Block>) {
        let name = type_name.into();
        self.blocks.insert(name, block);
    }

    /// RegisterBlockFunc registers a synchronous inline handler function as a block.
    /// The block is also pre-resolved so it is available via `call_block()`.
    ///
    /// For handlers that need to perform async work, use `register_block_func_async`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn register_block_func(
        &mut self,
        type_name: impl Into<String>,
        handler: impl Fn(&dyn crate::context::Context, &mut Message) -> Result_ + Send + Sync + 'static,
    ) {
        use crate::block::BlockInfo;
        let name = type_name.into();
        let block: Arc<dyn Block> = Arc::new(FuncBlock {
            info: BlockInfo::new(name.clone(), "0.0.0", "inline", "Inline function block"),
            handler: Box::new(handler),
        });
        self.register_block(name, block);
    }

    /// RegisterBlockFunc (wasm32 variant — no Send + Sync bounds).
    #[cfg(target_arch = "wasm32")]
    pub fn register_block_func(
        &mut self,
        type_name: impl Into<String>,
        handler: impl Fn(&dyn crate::context::Context, &mut Message) -> Result_ + 'static,
    ) {
        use crate::block::BlockInfo;
        let name = type_name.into();
        let block: Arc<dyn Block> = Arc::new(FuncBlock {
            info: BlockInfo::new(name.clone(), "0.0.0", "inline", "Inline function block"),
            handler: Box::new(handler),
        });
        self.register_block(name, block);
    }

    /// RegisterBlockFuncAsync registers an async inline handler function as a block.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn register_block_func_async<F, Fut>(
        &mut self,
        type_name: impl Into<String>,
        handler: F,
    ) where
        F: for<'a> Fn(&'a dyn crate::context::Context, &'a mut Message) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result_> + Send + 'static,
    {
        use crate::block::{AsyncFuncBlock, BlockInfo};
        let name = type_name.into();
        let block: Arc<dyn Block> = Arc::new(AsyncFuncBlock {
            info: BlockInfo::new(name.clone(), "0.0.0", "inline-async", "Inline async function block"),
            handler: Box::new(move |ctx, msg| Box::pin(handler(ctx, msg))),
        });
        self.register_block(name, block);
    }

    /// RegisterBlockFuncAsync (wasm32 variant — Sync only, no Send).
    #[cfg(target_arch = "wasm32")]
    pub fn register_block_func_async<F, Fut>(
        &mut self,
        type_name: impl Into<String>,
        handler: F,
    ) where
        F: for<'a> Fn(&'a dyn crate::context::Context, &'a mut Message) -> Fut + Sync + 'static,
        Fut: std::future::Future<Output = Result_> + 'static,
    {
        use crate::block::{AsyncFuncBlock, BlockInfo};
        let name = type_name.into();
        let block: Arc<dyn Block> = Arc::new(AsyncFuncBlock {
            info: BlockInfo::new(name.clone(), "0.0.0", "inline-async", "Inline async function block"),
            handler: Box::new(move |ctx, msg| Box::pin(handler(ctx, msg))),
        });
        self.register_block(name, block);
    }

    /// Shorthand for [`register_block_func`](Self::register_block_func).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn register_func(
        &mut self,
        type_name: impl Into<String>,
        handler: impl Fn(&dyn crate::context::Context, &mut Message) -> Result_ + Send + Sync + 'static,
    ) {
        self.register_block_func(type_name, handler);
    }

    #[cfg(target_arch = "wasm32")]
    pub fn register_func(
        &mut self,
        type_name: impl Into<String>,
        handler: impl Fn(&dyn crate::context::Context, &mut Message) -> Result_ + 'static,
    ) {
        self.register_block_func(type_name, handler);
    }

    /// Shorthand for [`register_block_func_async`](Self::register_block_func_async).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn register_func_async<F, Fut>(
        &mut self,
        type_name: impl Into<String>,
        handler: F,
    ) where
        F: for<'a> Fn(&'a dyn crate::context::Context, &'a mut Message) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result_> + Send + 'static,
    {
        self.register_block_func_async(type_name, handler);
    }

    #[cfg(target_arch = "wasm32")]
    pub fn register_func_async<F, Fut>(
        &mut self,
        type_name: impl Into<String>,
        handler: F,
    ) where
        F: for<'a> Fn(&'a dyn crate::context::Context, &'a mut Message) -> Fut + Sync + 'static,
        Fut: std::future::Future<Output = Result_> + 'static,
    {
        self.register_block_func_async(type_name, handler);
    }

    /// Register a WaferFlow definition.
    pub fn add_flow(&mut self, flow: wafer_flow::WaferFlow) {
        self.flows.insert(flow.id.clone(), flow);
    }

    /// Parse, validate, and register a WaferFlow from a JSON string.
    pub fn add_flow_json(&mut self, json: &str) -> Result<(), String> {
        let flow = wafer_flow::parse(json).map_err(|e| e.to_string())?;
        wafer_flow::validate(&flow).map_err(|errors| {
            errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        self.add_flow(flow);
        Ok(())
    }
}
