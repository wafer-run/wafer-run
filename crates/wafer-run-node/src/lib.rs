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
    use wafer_run::{Message, SealState, StaticConfigSource, Wafer};

    /// The WAFER runtime, exposed as a JavaScript class.
    ///
    /// Usage from Node.js / TypeScript (`test/smoke.test.cjs` runs this
    /// example):
    /// ```js
    /// const { WaferRuntime } = require('wafer-run');
    /// const w = new WaferRuntime();
    /// // A WASM block registers under the name its guest reports, here the
    /// // repo's examples/wasmi-block; registerBlock also grants capabilities.
    /// await w.registerBlock('example/echo', './echo_block.wasm', '{"crypto":true}');
    /// // A flow registers under the id in its JSON.
    /// await w.register('main', './main-flow.json');
    /// await w.start();
    /// const message = { kind: 'test', meta: [{ key: 'x-request-id', value: '1' }] };
    /// const result = JSON.parse(await w.run('main', JSON.stringify(message)));
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
        /// If `path` ends with `.wasm`, registers a WASM block with the given name,
        /// which must be the name the block reports in its `BlockInfo` (a mismatch
        /// is refused). Such a block runs with no capabilities, whatever it
        /// declares; `registerBlock` grants it some.
        /// Otherwise, reads the file as a JSON flow definition.
        #[napi]
        pub async fn register(&self, name: String, path: String) -> Result<()> {
            let mut inner = self.inner.write().await;
            wafer_run::embed::register_path(&mut inner, &name, &path)
                .map_err(Error::from_reason)?;
            drop(inner);
            Ok(())
        }

        /// Register the WASM block at `path` under `name` (the name the block
        /// reports in its `BlockInfo`), bounded by `capabilities`: a JSON
        /// `BlockCapabilities` object such as
        /// `{"collections":{"Only":["acme__widget__items"]},"crypto":true}`.
        /// An allowlist field is `"None"`, `"Any"` or `{"Only":[...]}`; a flag
        /// is a boolean. The block runs under that bound intersected with
        /// what it declares. A field the object omits denies, so `{}` grants
        /// nothing. Rejects on invalid JSON.
        #[napi]
        pub async fn register_block(
            &self,
            name: String,
            path: String,
            capabilities: String,
        ) -> Result<()> {
            let mut inner = self.inner.write().await;
            wafer_run::embed::register_block_path(&mut inner, &name, &path, &capabilities)
                .map_err(Error::from_reason)?;
            drop(inner);
            Ok(())
        }

        /// Finalize runtime configuration (composite config expansion, capability
        /// resolution, snapshot finalization). Block `Init` is dispatched lazily
        /// on first request. A runtime is sealed once: a second `resolve()`
        /// fails.
        #[napi]
        pub async fn resolve(&self) -> Result<()> {
            self.inner
                .write()
                .await
                .seal()
                .await
                .map_err(|e| Error::from_reason(e.to_string()))
        }

        /// Start the runtime. Calls `seal()` if `resolve()` has not; if
        /// `resolve()` failed, rejects with that failure rather than starting a
        /// runtime that never finished sealing.
        ///
        /// Uses `seal()` (no `bind()` on blocks) because the Node.js dev server
        /// has its own HTTP handling — blocks that spawn listeners are not needed
        /// here.
        ///
        /// Per-block `lifecycle(Init)` runs lazily on first dispatch per isolate
        /// — `start()` does not eagerly dispatch Init.
        #[napi]
        pub async fn start(&self) -> Result<()> {
            let mut inner = self.inner.write().await;
            match inner.seal_state().clone() {
                SealState::Unsealed => inner
                    .seal()
                    .await
                    .map_err(|e| Error::from_reason(e.to_string()))?,
                SealState::Sealed => {}
                SealState::Failed(reason) => return Err(Error::from_reason(reason)),
            }
            drop(inner);
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
            self.inner.write().await.shutdown().await;
            self.started.store(false, Ordering::Relaxed);
        }

        /// Run a flow with the given message (body-less).
        ///
        /// Takes the flow ID and a JSON message string,
        /// `{"kind":"...","meta":[{"key":"...","value":"..."}]}` (both fields
        /// required; `meta` may be `[]`). Returns a JSON result string:
        /// `{"action":"respond|drop|error|continue|halt", ...,"meta":{...}}`.
        /// `respond` carries `body` (UTF-8) or `body_base64` (other bytes),
        /// `halt` always `body_base64`, `error`
        /// `{"error":{"code":"...","message":"...","detail_code":"..."}}`
        /// (`detail_code` only when set), `continue` the follow-up message's
        /// `kind`; `drop` is a bodiless 204 whose `meta` holds only headers and
        /// cookies.
        ///
        /// `meta` holds only response entries, all string-valued, for the
        /// caller to apply to its response: `resp.status`, `resp.content_type`,
        /// `resp.header.{name}` (compare `{name}` case-insensitively; any case
        /// of `set-cookie` is one `Set-Cookie` directive) and
        /// `resp.set_cookie.{id}` — one `Set-Cookie` directive whose value is
        /// the whole directive (`sid=abc; Path=/; HttpOnly`), emitted as its
        /// own `Set-Cookie` header, never joined. `{id}` only keeps two
        /// cookies' keys apart (a block's cookie helpers write
        /// `{name}[;Domain={d}][;Path={p}]`, e.g. `resp.set_cookie.sid;Path=/api`);
        /// read nothing from it. Request state (headers, cookies, caller
        /// identity, client IP, query) never crosses this boundary, even when
        /// the block built its terminal from the request message.
        #[napi]
        pub async fn run(&self, flow_id: String, message_json: String) -> Result<String> {
            let msg: Message = serde_json::from_str(&message_json)
                .map_err(|e| Error::from_reason(format!("invalid Message JSON: {e}")))?;

            // The read guard is held until the result is encoded: a flow's
            // streamed response is still being produced by its block while
            // it is encoded, and `stop()` must not stop the block under it.
            let inner = self.inner.read().await;
            let output = inner
                .run(&flow_id, msg, wafer_run::InputStream::empty())
                .await;
            let json = wafer_run::embed::output_to_json(output).await;
            drop(inner);
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

    #[cfg(test)]
    mod tests {
        use std::{
            sync::{atomic::AtomicBool, Arc},
            time::Duration,
        };

        use tokio::sync::{Notify, RwLock};
        use wafer_run::{
            Block, BlockInfo, Context, InputStream, InstanceMode, Message, MetaEntry, OutputStream,
            StaticConfigSource, Wafer,
        };

        use super::WaferRuntime;

        /// Streams SSE: `one`, then waits for `gate`, then `two` and `Complete`.
        /// `opened` is notified once `one` is sent.
        struct GatedSse {
            opened: Arc<Notify>,
            gate: Arc<Notify>,
        }

        #[async_trait::async_trait]
        impl Block for GatedSse {
            fn info(&self) -> BlockInfo {
                BlockInfo::new("test/sse", "0.0.1", "http-handler@v1", "gated SSE fixture")
                    .instance_mode(InstanceMode::Singleton)
            }

            async fn handle(
                &self,
                _ctx: &dyn Context,
                _msg: Message,
                _input: InputStream,
            ) -> OutputStream {
                let (opened, gate) = (self.opened.clone(), self.gate.clone());
                OutputStream::from_producer(move |sink, _cancel| async move {
                    let _ = sink
                        .send_meta(MetaEntry {
                            key: "resp.content_type".into(),
                            value: "text/event-stream".into(),
                        })
                        .await;
                    let _ = sink.send_chunk(b"one;".to_vec()).await;
                    opened.notify_one();
                    gate.notified().await;
                    let _ = sink.send_chunk(b"two".to_vec()).await;
                    let _ = sink.complete(Vec::new()).await;
                })
            }
        }

        /// `stop()` waits for a `run` whose flow is still streaming its response
        /// from a block: the runtime is not shut down under the producer.
        #[tokio::test]
        async fn stop_waits_for_a_run_still_streaming_its_response() {
            let (opened, gate) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
            let mut wafer = Wafer::new(Arc::new(StaticConfigSource::default())).expect("wafer");
            wafer
                .register_block(
                    "test/sse",
                    Arc::new(GatedSse {
                        opened: opened.clone(),
                        gate: gate.clone(),
                    }),
                )
                .expect("register");
            wafer
                .add_flow_json(
                    r#"{ "id": "sse", "name": "sse", "version": "0.0.1",
                         "steps": [ { "id": "events", "block": "test/sse" } ] }"#,
                )
                .expect("flow");
            let rt = WaferRuntime {
                inner: Arc::new(RwLock::new(wafer)),
                started: Arc::new(AtomicBool::new(false)),
            };
            rt.start().await.expect("start");

            let run = rt.run("sse".into(), r#"{"kind":"test","meta":[]}"#.into());
            let stop_after_head = async {
                opened.notified().await;
                let stop = rt.stop();
                tokio::pin!(stop);
                assert!(
                    tokio::time::timeout(Duration::from_millis(300), &mut stop)
                        .await
                        .is_err(),
                    "stop() must wait for the run still encoding a streamed response"
                );
                gate.notify_one();
                stop.await;
            };
            let (json, ()) = tokio::join!(run, stop_after_head);
            let json: serde_json::Value = serde_json::from_str(&json.expect("run")).expect("JSON");
            assert_eq!(json["action"], "respond", "{json}");
            assert_eq!(json["body"], "one;two", "{json}");
        }
    }
}
