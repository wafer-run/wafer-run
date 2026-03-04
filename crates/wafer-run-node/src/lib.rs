//! wafer-run-node — Node.js native addon for the WAFER runtime via napi-rs.
//!
//! This calls wafer-run directly (no C FFI hop) for maximum efficiency.
//! All complex data crosses the boundary as JSON strings.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

use wafer_run::{FlowDef, Message, Wafer, WASMBlock};

/// The WAFER runtime, exposed as a JavaScript class.
///
/// Usage from Node.js / TypeScript:
/// ```js
/// const { WaferRuntime } = require('wafer-run');
/// const w = new WaferRuntime();
/// w.register('my-block', './block.wasm');
/// w.register('main', './main-flow.json');
/// w.resolve();
/// w.start();
/// const result = JSON.parse(w.run('main', JSON.stringify({ kind: 'test', data: '', meta: {} })));
/// ```
#[napi]
pub struct WaferRuntime {
    inner: Wafer,
}

impl Drop for WaferRuntime {
    fn drop(&mut self) {
        self.inner.stop();
    }
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

    /// Register a block or flow definition from a file path.
    ///
    /// If `path` ends with `.wasm`, registers a WASM block with the given name.
    /// Otherwise, reads the file as a JSON flow definition.
    #[napi]
    pub fn register(&mut self, name: String, path: String) -> Result<()> {
        if path.ends_with(".wasm") {
            let block = WASMBlock::load(&path)
                .map_err(|e| Error::from_reason(format!("failed to load WASM block: {}", e)))?;
            self.inner.register_block(&name, Arc::new(block));
        } else {
            let json = std::fs::read_to_string(&path)
                .map_err(|e| Error::from_reason(format!("failed to read file: {}", e)))?;
            let def: FlowDef = serde_json::from_str(&json)
                .map_err(|e| Error::from_reason(format!("invalid FlowDef JSON: {}", e)))?;
            self.inner.add_flow_def(&def);
        }
        Ok(())
    }

    /// Resolve all block references in registered flows.
    #[napi]
    pub fn resolve(&mut self) -> Result<()> {
        self.inner
            .resolve()
            .map_err(|e| Error::from_reason(e))
    }

    /// Start the runtime. Calls resolve() if not already resolved.
    ///
    /// Uses `start_without_bind()` because the Node.js dev server has its
    /// own HTTP handling — blocks that spawn listeners are not needed here.
    #[napi]
    pub fn start(&mut self) -> Result<()> {
        self.inner
            .start_without_bind()
            .map_err(|e| Error::from_reason(e))
    }

    /// Stop the runtime and shut down all block instances.
    #[napi]
    pub fn stop(&mut self) {
        self.inner.stop();
    }

    /// Run a flow with the given message.
    ///
    /// Takes the flow ID and a JSON message string. Returns a JSON result string:
    /// `{"action":"continue|respond|drop|error","response":{...},"error":{...}}`
    #[napi]
    pub fn run(&self, flow_id: String, message_json: String) -> Result<String> {
        let mut msg: Message = serde_json::from_str(&message_json)
            .map_err(|e| Error::from_reason(format!("invalid Message JSON: {}", e)))?;

        let result = self.inner.execute(&flow_id, &mut msg);

        serde_json::to_string(&result)
            .map_err(|e| Error::from_reason(format!("failed to serialize result: {}", e)))
    }

    /// Get info about all registered flows as a JSON array.
    #[napi]
    pub fn flows_info(&self) -> Result<String> {
        let info = self.inner.flows_info();
        serde_json::to_string(&info)
            .map_err(|e| Error::from_reason(format!("failed to serialize flows info: {}", e)))
    }

    /// Check whether a block type is registered.
    #[napi]
    pub fn has_block(&self, type_name: String) -> bool {
        self.inner.has_block(&type_name)
    }
}
