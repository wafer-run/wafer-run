use std::sync::Arc;

use super::Wafer;
use crate::{block::Block, error::RuntimeError};

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
    pub fn register(&mut self, name: &str, config: serde_json::Value) -> Result<(), RuntimeError> {
        if let Some(registrar) = self.registrars.remove(name) {
            registrar(self, config);
            self.registrars.insert(name.to_string(), registrar);
            return Ok(());
        }

        // No registrar — store config for deferred resolution during resolve().
        // The name must look like a remote ref (org/block).
        if !name.contains('/') {
            return Err(RuntimeError::Config(format!(
                "no registrar found for {name:?} and name is not a remote ref"
            )));
        }
        tracing::debug!(name = %name, "no registrar found, deferring to resolve()");
        self.add_block_config(name, config);
        Ok(())
    }

    /// Load block configurations from a JSON file.
    ///
    /// The file should be a JSON object mapping block names to config objects.
    /// Environment variables in `${VAR}` format are expanded before parsing.
    ///
    /// Native-only: requires filesystem access.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_blocks_json(&mut self, path: &str) -> Result<(), RuntimeError> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| RuntimeError::Config(format!("read blocks.json {path}: {e}")))?;

        let expanded = crate::helpers::expand_env_vars(&data);

        let mut map: std::collections::HashMap<String, serde_json::Value> =
            serde_json::from_str(&expanded)
                .map_err(|e| RuntimeError::Config(format!("parse blocks.json: {e}")))?;

        // Extract alias definitions before processing block configs
        if let Some(aliases_val) = map.remove("aliases") {
            if let Some(aliases_obj) = aliases_val.as_object() {
                for (alias, target) in aliases_obj {
                    if let Some(target_str) = target.as_str() {
                        Arc::make_mut(&mut self.aliases)
                            .insert(alias.clone(), target_str.to_string());
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
        self.config_expanders
            .insert(name.into(), Box::new(expander));
    }

    #[cfg(target_arch = "wasm32")]
    pub fn add_config_expander(
        &mut self,
        name: impl Into<String>,
        expander: impl Fn(serde_json::Value) -> Vec<(String, serde_json::Value)> + 'static,
    ) {
        self.config_expanders
            .insert(name.into(), Box::new(expander));
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
    ///
    /// If a host-side asset loader has been registered via
    /// [`set_asset_loader`](super::Wafer::set_asset_loader), it is
    /// propagated to WASM blocks at registration time so that
    /// `set_asset_loader` and `register_block` can be called in any order.
    pub fn register_block(
        &mut self,
        type_name: impl Into<String>,
        block: Arc<dyn Block>,
    ) -> Result<(), RuntimeError> {
        let name = type_name.into();
        super::validate_block_name(&name)?;
        if self.blocks.contains_key(&name) {
            return Err(RuntimeError::DuplicateBlock { name });
        }

        // Propagate the current asset loader to the block before inserting.
        // Only WasmiBlock instances override `as_any()`, so native blocks are
        // skipped without any unsafe code.
        #[cfg(feature = "wasmi")]
        if let Some(wasmi_block) = block
            .as_any()
            .and_then(|any| any.downcast_ref::<crate::wasm::WasmiBlock>())
        {
            wasmi_block.set_asset_loader(self.asset_loader.clone());
        }

        self.blocks.insert(name, block);
        Ok(())
    }

    /// Register a WaferFlow definition.
    pub fn add_flow(&mut self, flow: wafer_flow::WaferFlow) {
        self.flows.insert(flow.id.clone(), flow);
    }

    /// Parse, validate, and register a WaferFlow from a JSON string.
    pub fn add_flow_json(&mut self, json: &str) -> Result<(), RuntimeError> {
        let flow = wafer_flow::parse(json).map_err(|e| RuntimeError::Flow(e.to_string()))?;
        wafer_flow::validate(&flow).map_err(|errors| {
            RuntimeError::Flow(
                errors
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;
        let flow_id = flow.id.clone();
        self.add_flow(flow);
        tracing::info!(
            target: "wafer.runtime",
            event = "flow_registered",
            flow = %flow_id,
            "registered flow"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wafer_block::{
        streams::{input::InputStream, output::OutputStream},
        types::BlockInfo,
        Message, WaferError,
    };

    use crate::Wafer;

    struct NoopBlock;

    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    impl crate::block::Block for NoopBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("noop", "0.0.1", "noop.handle", "noop block for testing")
        }
        async fn handle(
            &self,
            _ctx: &dyn wafer_block::context::Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::respond(vec![])
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn wafer_block::context::Context,
            _event: wafer_block::LifecycleEvent,
        ) -> std::result::Result<(), WaferError> {
            Ok(())
        }
    }

    #[test]
    fn register_block_rejects_invalid_name() {
        let mut wafer = Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer build is infallible");
        let block = Arc::new(NoopBlock);
        assert!(wafer.register_block("my_org/block", block.clone()).is_err());
        assert!(wafer.register_block("noSlash", block.clone()).is_err());
        assert!(wafer.register_block("my-org/block", block).is_ok());
    }
}
