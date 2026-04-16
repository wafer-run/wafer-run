//! wafer-run-node — Node.js native addon for the WAFER runtime via napi-rs.
//!
//! This calls wafer-run directly (no C FFI hop) for maximum efficiency.
//! All complex data crosses the boundary as JSON strings.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

use wafer_run::{Message, Wafer, WasmiBlock};

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
    /// Tokio runtime for bridging async calls at the NAPI boundary.
    rt: tokio::runtime::Runtime,
}

impl Drop for WaferRuntime {
    fn drop(&mut self) {
        self.rt.block_on(self.inner.stop());
    }
}

#[napi]
impl WaferRuntime {
    /// Create a new WAFER runtime instance.
    #[napi(constructor)]
    pub fn new() -> Result<Self> {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| Error::from_reason(format!("failed to create tokio runtime: {e}")))?;
        Ok(Self {
            inner: Wafer::new(),
            rt,
        })
    }

    /// Register a block or flow definition from a file path.
    ///
    /// If `path` ends with `.wasm`, registers a WASM block with the given name.
    /// Otherwise, reads the file as a JSON flow definition.
    #[napi]
    pub fn register(&mut self, name: String, path: String) -> Result<()> {
        if path.ends_with(".wasm") {
            let block = WasmiBlock::load(&path)
                .map_err(|e| Error::from_reason(format!("failed to load WASM block: {e}")))?;
            self.inner
                .register_block(&name, Arc::new(block))
                .map_err(|e| Error::from_reason(e.to_string()))?;
        } else {
            let json = std::fs::read_to_string(&path)
                .map_err(|e| Error::from_reason(format!("failed to read file: {e}")))?;
            self.inner
                .add_flow_json(&json)
                .map_err(|e| Error::from_reason(format!("invalid WaferFlow JSON: {e}")))?;
        }
        Ok(())
    }

    /// Resolve all block references in registered flows.
    #[napi]
    pub fn resolve(&mut self) -> Result<()> {
        self.rt
            .block_on(self.inner.resolve())
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Start the runtime. Calls resolve() if not already resolved.
    ///
    /// Uses `start_without_bind()` because the Node.js dev server has its
    /// own HTTP handling — blocks that spawn listeners are not needed here.
    #[napi]
    pub fn start(&mut self) -> Result<()> {
        self.rt
            .block_on(self.inner.start_without_bind())
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Stop the runtime and shut down all block instances.
    #[napi]
    pub fn stop(&mut self) {
        self.rt.block_on(self.inner.stop());
    }

    /// Run a flow with the given message (body-less).
    ///
    /// Takes the flow ID and a JSON message string. Returns a JSON result string:
    /// `{"action":"respond|drop|error|continue","body":"...","meta":{...}}`
    #[napi]
    pub fn run(&self, flow_id: String, message_json: String) -> Result<String> {
        let msg: Message = serde_json::from_str(&message_json)
            .map_err(|e| Error::from_reason(format!("invalid Message JSON: {e}")))?;

        let input = wafer_run::InputStream::empty();
        let output = self.rt.block_on(self.inner.run(&flow_id, msg, input));

        // Collect the streaming output to a buffered JSON response.
        let json = self.rt.block_on(async {
            match output.collect_buffered().await {
                Ok(buf) => {
                    let body_str = String::from_utf8(buf.body).unwrap_or_default();
                    let meta_obj: serde_json::Value = buf
                        .meta
                        .iter()
                        .map(|e| (e.key.clone(), serde_json::Value::String(e.value.clone())))
                        .collect::<serde_json::Map<_, _>>()
                        .into();
                    serde_json::json!({
                        "action": "respond",
                        "body": body_str,
                        "meta": meta_obj,
                    })
                    .to_string()
                }
                Err(wafer_block::streams::output::TerminalNotResponse::Error(err)) => {
                    serde_json::json!({
                        "action": "error",
                        "error": {
                            "code": format!("{:?}", err.code),
                            "message": err.message,
                        }
                    })
                    .to_string()
                }
                Err(wafer_block::streams::output::TerminalNotResponse::Drop) => {
                    serde_json::json!({ "action": "drop" }).to_string()
                }
                Err(wafer_block::streams::output::TerminalNotResponse::Continue(msg)) => {
                    serde_json::json!({
                        "action": "continue",
                        "message": serde_json::to_value(&msg).unwrap_or_default(),
                    })
                    .to_string()
                }
                Err(wafer_block::streams::output::TerminalNotResponse::Malformed) => {
                    serde_json::json!({
                        "action": "error",
                        "error": { "code": "Internal", "message": "stream ended without terminal event" }
                    })
                    .to_string()
                }
            }
        });

        Ok(json)
    }

    /// Get info about all registered flows as a JSON array.
    #[napi]
    pub fn flows_info(&self) -> Result<String> {
        let info = self.inner.flows_info();
        serde_json::to_string(&info)
            .map_err(|e| Error::from_reason(format!("failed to serialize flows info: {e}")))
    }

    /// Check whether a block type is registered.
    #[napi]
    pub fn has_block(&self, type_name: String) -> bool {
        self.inner.has_block(&type_name)
    }
}

/// Validate a WaferFlow JSON definition.
///
/// Returns `null` on success, or a string describing validation errors.
#[napi]
pub fn validate_waferflow(json: String) -> Result<Option<String>> {
    let flow = match wafer_flow::parse(&json) {
        Ok(f) => f,
        Err(e) => return Ok(Some(format!("parse error: {e}"))),
    };
    match wafer_flow::validate(&flow) {
        Ok(()) => Ok(None),
        Err(errors) => {
            let msg = errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            Ok(Some(msg))
        }
    }
}
