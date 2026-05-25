//! wafer-run-node — Node.js native addon for the WAFER runtime via napi-rs.
//!
//! All runtime methods are async; napi-rs emits them as `Promise`-returning JS
//! methods. There is no `block_on` bridge — the JS event loop drives tokio.

#![warn(missing_docs)]

pub use bindings::{validate_waferflow, WaferRuntime};

// napi-derive `#[napi]` expands into extra impl blocks containing internal
// NAPI registration / boxing helpers (associated functions + methods) that
// have no source-level item we can attach doc comments to. Wrap the
// bindings in a private module so those macro-emitted items inherit
// `#[allow(missing_docs)]`; every item authored in this crate must still
// be documented (the lint stays active outside `bindings`).
#[allow(missing_docs)]
mod bindings {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use napi::bindgen_prelude::*;
    use napi_derive::napi;
    use tokio::sync::RwLock;
    use wafer_run::{Message, StaticConfigSource, Wafer, WasmiBlock};

    /// The WAFER runtime, exposed as a JavaScript class.
    ///
    /// Usage from Node.js / TypeScript:
    /// ```js
    /// const { WaferRuntime } = require('wafer-run');
    /// const w = new WaferRuntime();
    /// await w.register('my-block', './block.wasm');
    /// await w.register('main', './main-flow.json');
    /// await w.resolve();
    /// await w.start();
    /// const result = JSON.parse(await w.run('main', JSON.stringify({ kind: 'test', data: '', meta: {} })));
    /// await w.stop();
    /// ```
    #[napi]
    pub struct WaferRuntime {
        inner: Arc<RwLock<Wafer>>,
        started: Arc<AtomicBool>,
    }

    impl Drop for WaferRuntime {
        fn drop(&mut self) {
            if self.started.swap(false, Ordering::Relaxed) {
                // Block lifecycle(Stop) handlers won't run — they need a tokio
                // runtime, and Drop is sync. Calling `await w.stop()` before
                // letting the runtime go out of scope is the user's responsibility.
                eprintln!(
                    "wafer-run: WaferRuntime dropped without stop() — \
                 block shutdown lifecycles will not run; \
                 always `await runtime.stop()` before disposing the runtime"
                );
            }
        }
    }

    #[napi]
    impl WaferRuntime {
        /// Create a new WAFER runtime instance.
        #[napi(constructor)]
        pub fn new() -> Result<Self> {
            let inner = Wafer::new(Arc::new(StaticConfigSource::default())).map_err(|e| {
                Error::from_reason(format!("failed to initialise Wafer runtime: {e}"))
            })?;
            Ok(Self {
                inner: Arc::new(RwLock::new(inner)),
                started: Arc::new(AtomicBool::new(false)),
            })
        }

        /// Register a block or flow definition from a file path.
        ///
        /// If `path` ends with `.wasm`, registers a WASM block with the given name.
        /// Otherwise, reads the file as a JSON flow definition.
        #[napi]
        pub async fn register(&self, name: String, path: String) -> Result<()> {
            let mut inner = self.inner.write().await;
            if path.ends_with(".wasm") {
                let block = WasmiBlock::load(&path)
                    .map_err(|e| Error::from_reason(format!("failed to load WASM block: {e}")))?;
                inner
                    .register_block(&name, Arc::new(block))
                    .map_err(|e| Error::from_reason(e.to_string()))?;
            } else {
                let json = std::fs::read_to_string(&path)
                    .map_err(|e| Error::from_reason(format!("failed to read file: {e}")))?;
                inner
                    .add_flow_json(&json)
                    .map_err(|e| Error::from_reason(format!("invalid WaferFlow JSON: {e}")))?;
            }
            Ok(())
        }

        /// Finalize runtime configuration (composite config expansion, capability
        /// resolution, snapshot finalization). Block `Init` is dispatched lazily
        /// on first request. See [`wafer_run::Wafer::seal`].
        #[napi]
        pub async fn resolve(&self) -> Result<()> {
            self.inner
                .write()
                .await
                .seal()
                .await
                .map_err(|e| Error::from_reason(e.to_string()))
        }

        /// Start the runtime. Calls `seal()` if not already sealed.
        ///
        /// Uses `seal()` (no `bind()` on blocks) because the Node.js dev server
        /// has its own HTTP handling — blocks that spawn listeners are not needed
        /// here.
        ///
        /// Per-block `lifecycle(Init)` runs lazily on first dispatch per isolate
        /// — `start()` does not eagerly dispatch Init. See
        /// [`wafer_run::Wafer::seal`].
        #[napi]
        pub async fn start(&self) -> Result<()> {
            self.inner
                .write()
                .await
                .seal()
                .await
                .map_err(|e| Error::from_reason(e.to_string()))?;
            self.started.store(true, Ordering::Relaxed);
            Ok(())
        }

        /// Stop the runtime and shut down all block instances.
        ///
        /// Must be awaited before the runtime is garbage-collected so that block
        /// `lifecycle(Stop)` handlers can release resources (DB connections, file
        /// handles, etc.). Drop will log a warning if this method was not called.
        #[napi]
        pub async fn stop(&self) {
            self.inner.write().await.stop().await;
            self.started.store(false, Ordering::Relaxed);
        }

        /// Run a flow with the given message (body-less).
        ///
        /// Takes the flow ID and a JSON message string. Returns a JSON result string:
        /// `{"action":"respond|drop|error|continue","body":"...","meta":{...}}`
        #[napi]
        pub async fn run(&self, flow_id: String, message_json: String) -> Result<String> {
            let msg: Message = serde_json::from_str(&message_json)
                .map_err(|e| Error::from_reason(format!("invalid Message JSON: {e}")))?;

            let input = wafer_run::InputStream::empty();
            let output = {
                let inner = self.inner.read().await;
                inner.run(&flow_id, msg, input).await
            };

            let json = match output.collect_buffered().await {
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
                Err(wafer_block::streams::output::TerminalNotResponse::Halt(buf)) => {
                    let body_str = String::from_utf8(buf.body).unwrap_or_default();
                    let meta_obj: serde_json::Value = buf
                        .meta
                        .iter()
                        .map(|e| (e.key.clone(), serde_json::Value::String(e.value.clone())))
                        .collect::<serde_json::Map<_, _>>()
                        .into();
                    serde_json::json!({
                        "action": "halt",
                        "body": body_str,
                        "meta": meta_obj,
                    })
                    .to_string()
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
            };

            Ok(json)
        }

        /// Get info about all registered flows as a JSON array.
        #[napi]
        pub async fn flows_info(&self) -> Result<String> {
            let info = self.inner.read().await.flows_info();
            serde_json::to_string(&info)
                .map_err(|e| Error::from_reason(format!("failed to serialize flows info: {e}")))
        }

        /// Check whether a block type is registered.
        #[napi]
        pub async fn has_block(&self, type_name: String) -> bool {
            self.inner.read().await.has_block(&type_name)
        }
    }

    /// Validate a WaferFlow JSON definition.
    ///
    /// Returns `null` on success, or a string describing validation errors.
    /// CPU-only; stays sync.
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
}
