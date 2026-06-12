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
    use wafer_run::{Message, StaticConfigSource, Wafer};

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
        /// Otherwise, reads the file as a JSON flow definition. See
        /// [`wafer_run::embed::register_path`] for the dispatch rule.
        #[napi]
        pub async fn register(&self, name: String, path: String) -> Result<()> {
            let mut inner = self.inner.write().await;
            wafer_run::embed::register_path(&mut inner, &name, &path)
                .map_err(Error::from_reason)?;
            drop(inner);
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
        /// `{"action":"respond|drop|error|continue|halt","body":"...","meta":{...}}`
        ///
        /// The wire format (including the `body` vs `body_base64` rules for
        /// `respond` and `halt`) is documented on
        /// [`wafer_run::embed::output_to_json`], which produces it.
        #[napi]
        pub async fn run(&self, flow_id: String, message_json: String) -> Result<String> {
            let msg: Message = serde_json::from_str(&message_json)
                .map_err(|e| Error::from_reason(format!("invalid Message JSON: {e}")))?;

            let input = wafer_run::InputStream::empty();
            let output = {
                let inner = self.inner.read().await;
                inner.run(&flow_id, msg, input).await
            };

            Ok(wafer_run::embed::output_to_json(output).await)
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
    // `#[allow]` (not `#[expect]`): the `#[napi]` macro marshals the JS argument
    // into an owned `String`, so the binding requires an owned parameter and
    // `&str` is not an option here. The lint fires inside the macro expansion,
    // where an `#[expect]` on this fn is reported unfulfilled, so `allow` is the
    // only attribute that reliably suppresses it.
    #[allow(clippy::needless_pass_by_value)]
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
