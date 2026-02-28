//! wafer-run-node — Node.js native addon for the WAFER runtime via napi-rs.
//!
//! This calls wafer-run directly (no C FFI hop) for maximum efficiency.
//! All complex data crosses the boundary as JSON strings.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

use wafer_run::{ChainDef, Message, Wafer, WASMBlock};

/// The WAFER runtime, exposed as a JavaScript class.
///
/// Usage from Node.js / TypeScript:
/// ```js
/// const { WaferRuntime } = require('@anthropics/wafer-run-node');
/// const w = new WaferRuntime();
/// w.registerWasmBlock('my-block', './block.wasm');
/// w.addChainDef(JSON.stringify({ id: 'main', root: { block: 'my-block' } }));
/// w.resolve();
/// w.start();
/// const result = JSON.parse(w.execute('main', JSON.stringify({ kind: 'test', data: '', meta: {} })));
/// ```
#[napi]
pub struct WaferRuntime {
    inner: Wafer,
}

#[napi]
impl WaferRuntime {
    /// Create a new WAFER runtime instance.
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Wafer::new(),
        }
    }

    /// Register a WASM block from a file path.
    #[napi]
    pub fn register_wasm_block(&mut self, type_name: String, wasm_path: String) -> Result<()> {
        let block = WASMBlock::load(&wasm_path)
            .map_err(|e| Error::from_reason(format!("failed to load WASM block: {}", e)))?;
        self.inner.register_block(&type_name, Arc::new(block));
        Ok(())
    }

    /// Add a chain definition from a JSON string.
    #[napi]
    pub fn add_chain_def(&mut self, chain_def_json: String) -> Result<()> {
        let def: ChainDef = serde_json::from_str(&chain_def_json)
            .map_err(|e| Error::from_reason(format!("invalid ChainDef JSON: {}", e)))?;
        self.inner.add_chain_def(&def);
        Ok(())
    }

    /// Resolve all block references in registered chains.
    #[napi]
    pub fn resolve(&mut self) -> Result<()> {
        self.inner
            .resolve()
            .map_err(|e| Error::from_reason(e))
    }

    /// Start the runtime. Calls resolve() if not already resolved.
    #[napi]
    pub fn start(&mut self) -> Result<()> {
        self.inner
            .start()
            .map_err(|e| Error::from_reason(e))
    }

    /// Stop the runtime and shut down all block instances.
    #[napi]
    pub fn stop(&self) {
        self.inner.stop();
    }

    /// Execute a chain with the given message.
    ///
    /// Takes the chain ID and a JSON message string. Returns a JSON result string:
    /// `{"action":"continue|respond|drop|error","response":{...},"error":{...}}`
    #[napi]
    pub fn execute(&self, chain_id: String, message_json: String) -> Result<String> {
        let mut msg: Message = serde_json::from_str(&message_json)
            .map_err(|e| Error::from_reason(format!("invalid Message JSON: {}", e)))?;

        let result = self.inner.execute(&chain_id, &mut msg);

        serde_json::to_string(&result)
            .map_err(|e| Error::from_reason(format!("failed to serialize result: {}", e)))
    }

    /// Get info about all registered chains as a JSON array.
    #[napi]
    pub fn chains_info(&self) -> Result<String> {
        let info = self.inner.chains_info();
        serde_json::to_string(&info)
            .map_err(|e| Error::from_reason(format!("failed to serialize chains info: {}", e)))
    }

    /// Check whether a block type is registered.
    #[napi]
    pub fn has_block(&self, type_name: String) -> bool {
        self.inner.has_block(&type_name)
    }
}
