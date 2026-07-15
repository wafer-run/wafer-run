use std::sync::{Arc, Mutex};

use tracing::{debug, error};
use wafer_block::{
    core_types::*,
    error::RuntimeError,
    streams::{input::InputStream, output::OutputStream},
    types::*,
    Block,
};
use wafer_block_macro::wafer_async_trait;
use wasmi::{Engine, Linker, Module, Store, TypedResumableCall, Val};

use super::{capabilities::BlockCapabilities, host::ContextGuard, stream::StreamRegistry};
use crate::{
    context::Context,
    runtime::wasm_state::{FuelLimit, ResourceLimits},
};

mod abi;
mod imports;
mod meta;

use abi::*;
use imports::*;
use meta::*;

// ---------------------------------------------------------------------------
// Fuel application
// ---------------------------------------------------------------------------

/// Apply the configured [`FuelLimit`] to a freshly-prepared store.
///
/// `Metered(n)` sets the store's fuel to `n`; `Unmetered` is a no-op — wasmi
/// rejects `set_fuel` when the engine has `consume_fuel(false)`, so the loader
/// must skip it. `ctx` labels the error site (initial set vs. post-`_start`
/// refill).
fn apply_fuel(
    store: &mut Store<WasmiHostState>,
    fuel: FuelLimit,
    ctx: &str,
) -> Result<(), RuntimeError> {
    if let FuelLimit::Metered(n) = fuel {
        store
            .set_fuel(n)
            .map_err(|e| RuntimeError::Wasm(format!("{ctx}: {e}")))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Instantiation helper
// ---------------------------------------------------------------------------

/// Create a fresh store + instance from the pre-built linker and module.
///
/// For TinyGo WASM modules (wasi target) the exported `_start` function must
/// be called after instantiation to initialise the Go runtime (allocator,
/// goroutine scheduler, global vars) and to invoke `main()` (which calls
/// `wafer.Register`). Without it every WAFER export traps with `unreachable`.
///
/// `_start` terminates by calling `proc_exit(0)` — that traps with our
/// [`ProcExitTrap`] marker. We downcast the wasmi error to that typed marker:
/// `proc_exit(0)` is the expected WASI shutdown; a non-zero (or unrelated) trap
/// is surfaced as an error. Rust-compiled blocks have no `_start` export and
/// are unaffected.
fn instantiate(
    engine: &Engine,
    linker: &Linker<WasmiHostState>,
    module: &Module,
    caps: &BlockCapabilities,
    limits: ResourceLimits,
) -> Result<(Store<WasmiHostState>, wasmi::Instance), RuntimeError> {
    let host_state = WasmiHostState {
        context: None,
        max_memory_pages: limits.memory_pages,
        max_table_elements: limits.max_table_elements,
        capabilities: caps.clone(),
        streams: StreamRegistry::with_limits(limits.max_host_bytes, limits.max_live_streams),
        pending_stream_finish: None,
        pending_stream_read: None,
        pending_stream_take_error: None,
        pending_load_asset: None,
        current_attachments: None,
    };
    let mut store = Store::new(engine, host_state);

    // Resource limits — the `WasmiHostState` is the store's `ResourceLimiter`,
    // and its `max_memory_pages` field bounds `memory.grow`.
    store.limiter(|state| state);
    // Fuel metering — `Metered(n)` sets the per-call budget; `Unmetered` skips
    // `set_fuel` entirely (wasmi rejects it when `consume_fuel(false)`).
    apply_fuel(&mut store, limits.fuel, "setting fuel")?;

    let pre = linker
        .instantiate(&mut store, module)
        .map_err(|e| RuntimeError::Wasm(format!("instantiation: {e}")))?;
    let instance = pre
        .start(&mut store)
        .map_err(|e| RuntimeError::Wasm(format!("running start function: {e}")))?;

    // Call `_start` if exported — required for TinyGo WASM modules.
    if let Ok(start_fn) = instance.get_typed_func::<(), ()>(&store, "_start") {
        match start_fn.call(&mut store, ()) {
            Ok(()) => {}
            Err(e) => match e.downcast_ref::<ProcExitTrap>() {
                // proc_exit(0) is the normal WASI shutdown path — expected.
                Some(ProcExitTrap { code: 0 }) => {}
                // A non-zero exit, or any other trap, is a genuine startup failure.
                Some(ProcExitTrap { code }) => {
                    return Err(RuntimeError::Wasm(format!(
                        "WASM _start exited with non-zero code {code}"
                    )));
                }
                None => {
                    return Err(RuntimeError::Wasm(format!("WASM _start failed: {e}")));
                }
            },
        }
        // Re-fill fuel so the subsequent guest call has a full budget.
        apply_fuel(&mut store, limits.fuel, "refilling fuel after _start")?;
    }

    Ok((store, instance))
}

// ---------------------------------------------------------------------------
// ContextScope — RAII install/clear of the borrowed Context in the store
// ---------------------------------------------------------------------------

/// RAII guard that installs a borrowed [`Context`] into the wasmi store's
/// `context` slot for the duration of a single guest invocation and clears it
/// on drop — on *every* exit path (`?`, early `return Err`, the unhandled-trap
/// branch, or success).
///
/// The slot holds a cloned `Arc` produced from a [`ContextGuard`] that
/// `transmute`s a non-`'static` `&dyn Context`. If that `Arc` outlives the
/// guard it dereferences freed memory, so `ContextGuard::drop` asserts its
/// strong count is exactly 1. Clearing the slot here (which drops the store's
/// `Arc` clone) before the inner `ContextGuard` drops is what keeps that
/// assertion satisfiable. Doing it via `Drop` removes the need for manual
/// `store.data_mut().context = None` on each return path — a single missed
/// clear would crash the host.
struct ContextScope<'s> {
    store: &'s mut Store<WasmiHostState>,
    // The store's `context` Arc is cleared in `ContextScope::drop` (below),
    // which runs before any field is dropped — so by the time this
    // `ContextGuard` drops and asserts its strong count is 1, the store's clone
    // is already gone. Field order is not load-bearing for that invariant.
    _guard: ContextGuard,
}

impl<'s> ContextScope<'s> {
    /// Install `ctx` into the store's `context` slot and seed the per-call
    /// `current_attachments`. The slot is cleared when the returned scope is
    /// dropped.
    fn new(
        store: &'s mut Store<WasmiHostState>,
        ctx: &dyn Context,
        attachments: Option<std::collections::BTreeMap<String, wafer_block::Attachment>>,
    ) -> Self {
        let guard = ContextGuard::new(ctx);
        store.data_mut().context = Some(guard.as_arc());
        store.data_mut().current_attachments = attachments;
        Self {
            store,
            _guard: guard,
        }
    }

    /// Shared access to the underlying store.
    fn store(&self) -> &Store<WasmiHostState> {
        self.store
    }

    /// Mutable access to the underlying store.
    fn store_mut(&mut self) -> &mut Store<WasmiHostState> {
        self.store
    }
}

impl Drop for ContextScope<'_> {
    fn drop(&mut self) {
        // Clear the store's Arc clone before `_guard` drops so the strong-count
        // assertion in `ContextGuard::drop` holds on every exit path.
        self.store.data_mut().context = None;
    }
}

// ---------------------------------------------------------------------------
// WasmiBlock
// ---------------------------------------------------------------------------

/// `Block` implementation that runs a WASM module via the `wasmi` interpreter.
pub struct WasmiBlock {
    engine: Engine,
    module: Module,
    linker: Linker<WasmiHostState>,
    info_cache: Mutex<Option<BlockInfo>>,
    /// Interior-mutable capabilities field so the runtime can propagate the
    /// effective set (`declared ∩ config`) after `resolve()` without reloading
    /// the WASM module.  Reads are lock-free on uncontended RwLock; the write
    /// path is exercised at most once per block lifetime (startup).
    capabilities: parking_lot::RwLock<BlockCapabilities>,
    /// Warn-once flag for outbound stripped headers.
    warned_outbound: std::sync::atomic::AtomicBool,
    /// Warn-once flag for inbound stripped headers.
    warned_inbound: std::sync::atomic::AtomicBool,
    /// Host-side asset loader for external WASM/JS assets referenced by the
    /// block's `external_assets` manifest field. Defaults to `NoopAssetLoader`.
    /// Hosts inject a real loader via `set_asset_loader`.
    asset_loader: parking_lot::RwLock<Arc<dyn crate::asset_loader::LoadAssetCallback>>,
    /// Per-guest-call resource limits applied at every `instantiate()` — the
    /// wasmi fuel budget and the linear-memory page cap. Selected at load time
    /// (defaults to [`ResourceLimits::default`]: the bounded 100M fuel cap and
    /// the 256-page / 16 MiB memory cap). The fuel mode must match the
    /// `consume_fuel` flag of the block's [`Engine`] — the constructors keep
    /// the two in sync.
    limits: ResourceLimits,
}

// Safety: Engine, Module, Linker are Send+Sync in wasmi 0.44.
// The Mutex guards the cache.
unsafe impl Send for WasmiBlock {}
unsafe impl Sync for WasmiBlock {}

impl WasmiBlock {
    /// Read a WASM module from disk and compile it (native-only convenience wrapper).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(path: &str) -> Result<Self, RuntimeError> {
        let bytes = std::fs::read(path)
            .map_err(|e| RuntimeError::Wasm(format!("reading WASM file: {e}")))?;
        Self::load_from_bytes(&bytes)
    }

    /// Compile a WASM module from raw bytes with unrestricted host capabilities
    /// and the default per-call resource limits ([`ResourceLimits::default`]:
    /// 100M fuel, 256-page / 16 MiB memory).
    pub fn load_from_bytes(wasm_bytes: &[u8]) -> Result<Self, RuntimeError> {
        Self::load_from_bytes_with_limits(wasm_bytes, ResourceLimits::default())
    }

    /// Compile a WASM module from raw bytes with unrestricted host capabilities
    /// and an explicit per-call [`FuelLimit`] (default memory cap).
    ///
    /// Thin wrapper over [`load_from_bytes_with_limits`](Self::load_from_bytes_with_limits)
    /// for callers that only need to tune fuel. To raise the memory cap as
    /// well, pass a full [`ResourceLimits`].
    pub fn load_from_bytes_with_fuel(
        wasm_bytes: &[u8],
        fuel: FuelLimit,
    ) -> Result<Self, RuntimeError> {
        Self::load_from_bytes_with_limits(
            wasm_bytes,
            ResourceLimits {
                fuel,
                ..ResourceLimits::default()
            },
        )
    }

    /// Compile a WASM module from raw bytes with unrestricted host capabilities
    /// and explicit per-call [`ResourceLimits`] (fuel budget + linear-memory
    /// page cap).
    ///
    /// This is the single entry point for trusted single-user embedders (e.g.
    /// gizza's native CLI and browser runtime) to set both bounds in one call —
    /// read the runtime's configured limits back via
    /// [`Wafer::resource_limits`](crate::Wafer::resource_limits). Pass
    /// [`FuelLimit::Unmetered`] for heavy compute and/or a larger
    /// `memory_pages` for memory-heavy tools (e.g. the `syntect`+font
    /// code-screenshot tool needs ~24 MiB ≈ 384 pages). The freshly-created
    /// engine's `consume_fuel` flag is matched to the requested fuel mode.
    pub fn load_from_bytes_with_limits(
        wasm_bytes: &[u8],
        limits: ResourceLimits,
    ) -> Result<Self, RuntimeError> {
        Self::load_with_capabilities_and_limits(
            wasm_bytes,
            BlockCapabilities::unrestricted(),
            limits,
        )
    }

    /// Compile a WASM module with a custom capability set (filters host imports)
    /// and the default per-call resource limits ([`ResourceLimits::default`]).
    pub fn load_with_capabilities(
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
    ) -> Result<Self, RuntimeError> {
        Self::load_with_capabilities_and_limits(wasm_bytes, caps, ResourceLimits::default())
    }

    /// Compile a WASM module with a custom capability set and explicit per-call
    /// [`ResourceLimits`]. Creates a fresh engine whose `consume_fuel` flag
    /// matches the requested fuel mode.
    pub fn load_with_capabilities_and_limits(
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
        limits: ResourceLimits,
    ) -> Result<Self, RuntimeError> {
        let mut config = wasmi::Config::default();
        config.consume_fuel(limits.fuel.consume_fuel());
        let engine = Engine::new(&config);
        Self::load_with_engine_and_limits(&engine, wasm_bytes, caps, limits)
    }

    /// Compile a WASM module reusing an existing `wasmi::Engine` (lets callers
    /// share fuel config) with the default per-call resource limits
    /// ([`ResourceLimits::default`]).
    ///
    /// The passed-in engine must already have `consume_fuel(true)` (the default
    /// for engines created by this loader and by `Wafer::wasm_engine`).
    pub fn load_with_engine(
        engine: &Engine,
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
    ) -> Result<Self, RuntimeError> {
        Self::load_with_engine_and_limits(engine, wasm_bytes, caps, ResourceLimits::default())
    }

    /// Compile a WASM module reusing an existing `wasmi::Engine` with explicit
    /// per-call [`ResourceLimits`].
    ///
    /// The caller is responsible for ensuring the engine's `consume_fuel` flag
    /// matches `limits.fuel` (`true` for [`FuelLimit::Metered`], `false` for
    /// [`FuelLimit::Unmetered`]). `Wafer::wasm_engine` derives the engine's
    /// flag from the runtime's configured limit and passes matching `limits`
    /// here, so blocks loaded through the runtime stay consistent. (The memory
    /// cap is enforced per-store, so it needs no engine coordination.)
    pub fn load_with_engine_and_limits(
        engine: &Engine,
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
        limits: ResourceLimits,
    ) -> Result<Self, RuntimeError> {
        let module = Module::new(engine, wasm_bytes)
            .map_err(|e| RuntimeError::Wasm(format!("compiling WASM module: {e}")))?;
        let linker = build_linker(engine)?;
        Ok(Self {
            engine: engine.clone(),
            module,
            linker,
            info_cache: Mutex::new(None),
            capabilities: parking_lot::RwLock::new(caps),
            warned_outbound: std::sync::atomic::AtomicBool::new(false),
            warned_inbound: std::sync::atomic::AtomicBool::new(false),
            asset_loader: parking_lot::RwLock::new(Arc::new(crate::asset_loader::NoopAssetLoader)),
            limits,
        })
    }

    /// Replace the asset loader used by `__wafer_host_load_asset`. Called by
    /// hosts (e.g. the browser build) during startup to inject a real loader that
    /// fetches CDN assets, verifies sha256, and returns readiness.
    pub fn set_asset_loader(&self, loader: Arc<dyn crate::asset_loader::LoadAssetCallback>) {
        *self.asset_loader.write() = loader;
    }

    /// Return the currently active asset loader. Used by tests to verify that
    /// propagation from `Wafer::set_asset_loader` / `Wafer::register_block`
    /// has taken effect.
    #[cfg(test)]
    pub fn asset_loader_for_test(&self) -> Arc<dyn crate::asset_loader::LoadAssetCallback> {
        self.asset_loader.read().clone()
    }

    /// Variant of `Block::handle` that seeds inbound attachments visible to
    /// the guest via `__wafer_host_lookup_attachment`. Called by
    /// `RuntimeContext::call_block_with_attachments` when the callee is a
    /// wasmi block.
    pub(crate) async fn handle_with_attachments(
        &self,
        ctx: &dyn Context,
        msg: Message,
        input: InputStream,
        attachments: std::collections::BTreeMap<String, wafer_block::Attachment>,
    ) -> OutputStream {
        self.handle_inner(ctx, msg, input, Some(attachments)).await
    }

    /// Shared body of `handle` / `handle_with_attachments`. Serialises
    /// (msg, body) for the guest, drives `__wafer_handle` through the resume
    /// loop with the given attachments slot, and decodes the guest ABI result
    /// back into an `OutputStream`.
    async fn handle_inner(
        &self,
        ctx: &dyn Context,
        msg: Message,
        input: InputStream,
        attachments: Option<std::collections::BTreeMap<String, wafer_block::Attachment>>,
    ) -> OutputStream {
        let body = input.collect_to_bytes().await;

        // Sanitize inbound message meta before passing to WASM guest.
        let msg = {
            let mut stripped_in: Vec<String> = Vec::new();
            let caps_guard = self.capabilities.read();
            let sanitized_meta = sanitize_inbound_meta(msg.meta, &caps_guard, &mut stripped_in);
            drop(caps_guard);
            if !stripped_in.is_empty() {
                self.warn_once_stripped_inbound(&stripped_in);
            }
            Message {
                meta: sanitized_meta,
                ..msg
            }
        };

        let msg_bytes = match serde_json::to_vec(&(&msg, &body)) {
            Ok(b) => b,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::Internal,
                    format!("serializing message: {e}"),
                ));
            }
        };

        let result_bytes = match self
            .call_guest_resumable_with_attachments(ctx, attachments, |store, instance| {
                let alloc_fn = instance
                    .get_typed_func::<i32, i32>(&*store, "__wafer_alloc")
                    .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_alloc: {e}")))?;
                let handle_fn = instance
                    .get_typed_func::<(i32, i32), i64>(&*store, "__wafer_handle")
                    .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_handle: {e}")))?;
                let memory = instance.get_memory(&*store, "memory").ok_or_else(|| {
                    RuntimeError::Wasm("guest has no exported memory".to_string())
                })?;

                let ptr = write_guest_bytes(store, alloc_fn, memory, &msg_bytes)?;
                let len = msg_bytes.len() as i32;
                Ok((handle_fn, ptr as i32, len))
            })
            .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::Internal,
                    format!("WASM handle error: {e}"),
                ));
            }
        };

        // The guest returns a guest ABI format JSON. Map it back to OutputStream.
        #[derive(serde::Deserialize)]
        struct GuestAbiResult {
            action: String,
            response: Option<GuestAbiResponse>,
            error: Option<WaferError>,
            message: Option<Message>,
        }
        #[derive(serde::Deserialize)]
        struct GuestAbiResponse {
            data: Vec<u8>,
            #[serde(default)]
            meta: Vec<MetaEntry>,
        }

        match serde_json::from_slice::<GuestAbiResult>(&result_bytes) {
            Ok(result) => match result.action.as_str() {
                "Respond" => {
                    let (data, meta) = result
                        .response
                        .map(|r| {
                            let mut stripped: Vec<String> = Vec::new();
                            let caps_guard = self.capabilities.read();
                            let sanitized =
                                sanitize_outbound_meta(r.meta, &caps_guard, &mut stripped);
                            drop(caps_guard);
                            if !stripped.is_empty() {
                                self.warn_once_stripped_outbound(&stripped);
                            }
                            (r.data, sanitized)
                        })
                        .unwrap_or_default();
                    if meta.is_empty() {
                        OutputStream::respond(data)
                    } else {
                        OutputStream::respond_with_meta(data, meta)
                    }
                }
                "Error" => {
                    let e = result.error.unwrap_or_else(|| {
                        WaferError::new(
                            ErrorCode::Internal,
                            "WASM block returned error with no details",
                        )
                    });
                    OutputStream::error(e)
                }
                "Drop" => OutputStream::drop_request(),
                "Continue" => {
                    let msg = result.message.unwrap_or_else(|| Message::new("continue"));
                    OutputStream::continue_with(msg)
                }
                _ => OutputStream::error(WaferError::new(
                    ErrorCode::Internal,
                    format!("unknown action from WASM guest: {}", result.action),
                )),
            },
            Err(e) => OutputStream::error(WaferError::new(
                ErrorCode::Internal,
                format!("deserializing WASM handle result: {e}"),
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Call a guest function that returns a packed i64 (ptr << 32 | len),
    /// handling the call_block trap+resume loop.
    ///
    /// `setup` prepares the store/instance (writes args, returns the TypedFunc).
    /// When a `call_block` trap occurs the loop resolves it via `ctx` and resumes.
    async fn call_guest_resumable(
        &self,
        ctx: &dyn Context,
        setup: impl FnOnce(
            &mut Store<WasmiHostState>,
            wasmi::Instance,
        )
            -> Result<(wasmi::TypedFunc<(i32, i32), i64>, i32, i32), RuntimeError>,
    ) -> Result<Vec<u8>, RuntimeError> {
        self.call_guest_resumable_with_attachments(ctx, None, setup)
            .await
    }

    /// Variant of `call_guest_resumable` that seeds the wasmi store's
    /// `current_attachments` slot before the guest call begins. Used by
    /// `WasmiBlock::handle_with_attachments` so a wasmi callee's
    /// `__wafer_host_lookup_attachment` host import can find the attachments
    /// the caller provided via `Context::call_block_with_attachments`.
    async fn call_guest_resumable_with_attachments(
        &self,
        ctx: &dyn Context,
        attachments: Option<std::collections::BTreeMap<String, wafer_block::Attachment>>,
        setup: impl FnOnce(
            &mut Store<WasmiHostState>,
            wasmi::Instance,
        )
            -> Result<(wasmi::TypedFunc<(i32, i32), i64>, i32, i32), RuntimeError>,
    ) -> Result<Vec<u8>, RuntimeError> {
        let caps_snapshot = self.capabilities.read().clone();
        let (mut store, instance) = instantiate(
            &self.engine,
            &self.linker,
            &self.module,
            &caps_snapshot,
            self.limits,
        )?;

        // Install the borrowed context (and inbound attachments) for the
        // duration of this call. `ContextScope::drop` clears the store's
        // `context` slot on *every* exit path — `?`, early `return Err`, the
        // unhandled-trap branch, or success — so the strong-count assertion in
        // `ContextGuard::drop` always holds. From here on the store is reached
        // through `scope`.
        let mut scope = ContextScope::new(&mut store, ctx, attachments);

        let (func, arg0, arg1) = setup(scope.store_mut(), instance)?;

        let memory = instance
            .get_memory(scope.store(), "memory")
            .ok_or_else(|| RuntimeError::Wasm("guest has no exported memory".to_string()))?;

        // Initial call (resumable).
        let mut resumable = match func
            .call_resumable(scope.store_mut(), (arg0, arg1))
            .map_err(|e| RuntimeError::Wasm(format!("guest call failed: {e}")))?
        {
            TypedResumableCall::Finished(packed) => {
                let (ptr, len) = unpack_ptr_len(packed)?;
                return read_guest_bytes(scope.store(), memory, ptr, len);
            }
            TypedResumableCall::Resumable(inv) => inv,
        };

        // Resolve pending calls in a loop. Each branch resolves its pending
        // host call and yields the resume `Val` (the return value of the
        // trapped host function) plus a label for resume-error messages; the
        // single shared resume+match at the bottom of the loop drives the
        // guest to either completion or its next trap.
        loop {
            // Dispatch based on which pending field is set by the host import.
            let (resume_val, resumed_after): (Val, &str) = if let Some(handle) =
                scope.store_mut().data_mut().pending_stream_finish.take()
            {
                // Pull the request out of the StreamState, dispatch via
                // Context::call_block, install the resulting OutputStream on
                // the StreamState. Resume with i32 0 on success, negative
                // ErrorCode on failure.
                // Drain (target, msg, body) and any attachments accumulated
                // via __wafer_host_stream_attach. Both come off the same
                // StreamState; the attachments hand-off must happen before we
                // await the dispatch (we don't want to keep `&mut store`
                // borrowed across an await).
                let take_result = {
                    let data = scope.store_mut().data_mut();
                    let state = data.streams.get_mut(handle);
                    state.map(|s| {
                        let req = s.take_finish_request();
                        let atts = s.take_attachments();
                        (req, atts)
                    })
                };
                let resume_code: i32 = match take_result {
                    Some((Ok((target, msg, body)), attachments)) => {
                        debug!(
                            block = target,
                            body_len = body.len(),
                            attachments = attachments.len(),
                            "resolving stream_finish from WASM guest"
                        );
                        let input = if body.is_empty() {
                            InputStream::empty()
                        } else {
                            InputStream::from_bytes(body)
                        };
                        let out = if attachments.is_empty() {
                            ctx.call_block(&target, msg, input).await
                        } else {
                            ctx.call_block_with_attachments(&target, msg, input, attachments)
                                .await
                        };
                        if let Some(state) = scope.store_mut().data_mut().streams.get_mut(handle) {
                            state.finish_with_stream(out);
                        }
                        0
                    }
                    Some((Err(e), _attachments)) => {
                        let code = e.code;
                        if let Some(state) = scope.store_mut().data_mut().streams.get_mut(handle) {
                            state.record_error_and_close(e);
                        }
                        error_code_to_neg_i32(code)
                    }
                    None => error_code_to_neg_i32(ErrorCode::NotFound),
                };

                (Val::I32(resume_code), "stream_finish")
            } else if let Some(handle) = scope.store_mut().data_mut().pending_stream_read.take() {
                // Drive the response stream's next frame. On Chunk: allocate
                // guest memory + write bytes, resume with packed (ptr, len).
                // On end-of-stream: resume with 0. On error: resume with
                // negative ErrorCode sentinel (the guest can call take_error
                // for full details).
                let next = match scope.store_mut().data_mut().streams.get_mut(handle) {
                    Some(s) => s.next_chunk().await,
                    None => Err(WaferError::new(
                        ErrorCode::NotFound,
                        "unknown stream handle",
                    )),
                };

                let resume_packed: i64 = match next {
                    Ok(Some(bytes)) => {
                        let alloc_fn = instance
                            .get_typed_func::<i32, i32>(scope.store(), "__wafer_alloc")
                            .map_err(|e| {
                                RuntimeError::Wasm(format!(
                                    "getting __wafer_alloc for stream_read resume: {e}"
                                ))
                            })?;
                        let ptr = write_guest_bytes(scope.store_mut(), alloc_fn, memory, &bytes)?;
                        pack_ptr_len(ptr, bytes.len() as u32)
                    }
                    Ok(None) => 0,
                    Err(e) => error_code_to_neg_i64(e.code),
                };

                (Val::I64(resume_packed), "stream_read")
            } else if let Some(handle) = scope
                .store_mut()
                .data_mut()
                .pending_stream_take_error
                .take()
            {
                // Pop the StreamState's last_error, encode via rmp-serde,
                // allocate guest memory + write bytes, resume with packed
                // (ptr, len). Resume with 0 if no error is present.
                let err_opt = scope
                    .store_mut()
                    .data_mut()
                    .streams
                    .get_mut(handle)
                    .and_then(|s| s.take_error());

                let resume_packed: i64 = match err_opt {
                    Some(err) => {
                        let bytes = wafer_block::codec::encode(&err).map_err(|e| {
                            RuntimeError::Wasm(format!(
                                "encoding WaferError for stream_take_error: {e}"
                            ))
                        })?;
                        let alloc_fn = instance
                            .get_typed_func::<i32, i32>(scope.store(), "__wafer_alloc")
                            .map_err(|e| {
                                RuntimeError::Wasm(format!(
                                    "getting __wafer_alloc for stream_take_error resume: {e}"
                                ))
                            })?;
                        let ptr = write_guest_bytes(scope.store_mut(), alloc_fn, memory, &bytes)?;
                        pack_ptr_len(ptr, bytes.len() as u32)
                    }
                    None => 0,
                };

                (Val::I64(resume_packed), "stream_take_error")
            } else if let Some(asset_id) = scope.store_mut().data_mut().pending_load_asset.take() {
                debug!(asset = asset_id, "resolving load_asset from WASM guest");

                let loader = self.asset_loader.read().clone();
                let status = loader.load(&asset_id).await;
                let code: i32 = match status {
                    crate::asset_loader::AssetLoadStatus::Ready => 0,
                    crate::asset_loader::AssetLoadStatus::Pending => 1,
                    crate::asset_loader::AssetLoadStatus::Failed(_) => 2,
                };

                (Val::I32(code), "load_asset")
            } else {
                return Err(RuntimeError::Wasm(format!(
                    "guest trapped but no pending host call (host error: {})",
                    resumable.host_error()
                )));
            };

            // Resume with the computed value as the return value of the
            // trapped host function. wasmi's resumable.resume value IS the
            // return value — no re-entry into the host fn.
            match resumable
                .resume(scope.store_mut(), &[resume_val])
                .map_err(|e| {
                    RuntimeError::Wasm(format!("resuming guest after {resumed_after}: {e}"))
                })? {
                TypedResumableCall::Finished(packed) => {
                    let (ptr, len) = unpack_ptr_len(packed)?;
                    return read_guest_bytes(scope.store(), memory, ptr, len);
                }
                TypedResumableCall::Resumable(next) => {
                    resumable = next;
                }
            }
        }
    }

    fn warn_once_stripped_outbound(&self, names: &[String]) {
        use std::sync::atomic::Ordering;
        if self.warned_outbound.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::warn!(
            block = %self.info().name,
            direction = "outbound",
            stripped = ?names,
            "headers outside writable allowlist — stripped"
        );
    }

    fn warn_once_stripped_inbound(&self, names: &[String]) {
        use std::sync::atomic::Ordering;
        if self.warned_inbound.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::warn!(
            block = %self.info().name,
            direction = "inbound",
            stripped = ?names,
            "headers outside readable allowlist — stripped"
        );
    }
}

#[wafer_async_trait]
impl Block for WasmiBlock {
    fn info(&self) -> BlockInfo {
        // Check cache first.
        if let Ok(guard) = self.info_cache.lock() {
            if let Some(ref info) = *guard {
                return info.clone();
            }
        }

        // Sync instantiation: call __wafer_info.
        let result = (|| -> Result<BlockInfo, RuntimeError> {
            let caps_snapshot = self.capabilities.read().clone();
            let (mut store, instance) = instantiate(
                &self.engine,
                &self.linker,
                &self.module,
                &caps_snapshot,
                self.limits,
            )?;

            let info_fn = instance
                .get_typed_func::<(), i64>(&store, "__wafer_info")
                .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_info: {e}")))?;

            let memory = instance
                .get_memory(&store, "memory")
                .ok_or_else(|| RuntimeError::Wasm("guest has no exported memory".to_string()))?;

            let packed = info_fn
                .call(&mut store, ())
                .map_err(|e| RuntimeError::Wasm(format!("calling __wafer_info: {e}")))?;

            let (ptr, len) = unpack_ptr_len(packed)?;
            let bytes = read_guest_bytes(&store, memory, ptr, len)?;
            let info: BlockInfo = serde_json::from_slice(&bytes)
                .map_err(|e| RuntimeError::Wasm(format!("deserializing BlockInfo: {e}")))?;
            Ok(info)
        })();

        match result {
            Ok(mut info) => {
                info.runtime = wafer_block::BlockRuntime::Wasm;
                if let Ok(mut guard) = self.info_cache.lock() {
                    *guard = Some(info.clone());
                }
                info
            }
            Err(e) => {
                // A block that cannot report its own `BlockInfo` is a hard
                // failure, not a routine warning. The `Block::info()` contract
                // is infallible, so we must return *something* — but the
                // placeholder name "unknown" is exactly what the registry and
                // router key off, so a failed block silently registers under
                // "unknown", collides with any other failed block, and never
                // routes. Log at `error!` so the failure is visible in volume
                // rather than masquerading as a normal block. The failure is
                // deliberately NOT cached: a transient instantiation fault
                // (e.g. fuel exhaustion) can recover on a later call.
                error!(
                    "WasmiBlock::info() failed: {e}; registering with placeholder \
                     name \"unknown\" — this block will not route correctly and \
                     should be treated as a load failure"
                );
                BlockInfo::new("unknown", "0.0.0", "unknown", "failed to load info")
            }
        }
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        self.handle_inner(ctx, msg, input, None).await
    }

    async fn lifecycle(
        &self,
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        let event_bytes = serde_json::to_vec(&event).map_err(|e| {
            WaferError::new(
                ErrorCode::Internal,
                format!("serializing lifecycle event: {e}"),
            )
        })?;

        let result_bytes = self
            .call_guest_resumable(ctx, |store, instance| {
                let alloc_fn = instance
                    .get_typed_func::<i32, i32>(&*store, "__wafer_alloc")
                    .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_alloc: {e}")))?;
                let lifecycle_fn = instance
                    .get_typed_func::<(i32, i32), i64>(&*store, "__wafer_lifecycle")
                    .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_lifecycle: {e}")))?;
                let memory = instance.get_memory(&*store, "memory").ok_or_else(|| {
                    RuntimeError::Wasm("guest has no exported memory".to_string())
                })?;

                let ptr = write_guest_bytes(store, alloc_fn, memory, &event_bytes)?;
                let len = event_bytes.len() as i32;
                Ok((lifecycle_fn, ptr as i32, len))
            })
            .await
            .map_err(|e| {
                WaferError::new(ErrorCode::Internal, format!("WASM lifecycle error: {e}"))
            })?;

        // The guest returns a JSON-encoded Result<(), WaferError>.
        let result: std::result::Result<(), WaferError> = serde_json::from_slice(&result_bytes)
            .map_err(|e| {
                WaferError::new(
                    ErrorCode::Internal,
                    format!("deserializing WASM lifecycle result: {e}"),
                )
            })?;
        result
    }

    fn block_capabilities(&self) -> Option<BlockCapabilities> {
        Some(self.capabilities.read().clone())
    }

    fn runtime_capabilities_mut(&self, new: BlockCapabilities) {
        *self.capabilities.write() = new;
    }

    /// Expose `self` as `&dyn Any` so the runtime can downcast `Arc<dyn Block>`
    /// to `Arc<WasmiBlock>` and forward the host-side asset loader without
    /// importing `wafer-run` types into `wafer-block`.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
}

// ---------------------------------------------------------------------------
// Unit tests for interior-mutable capabilities update
// ---------------------------------------------------------------------------

#[cfg(test)]
mod capabilities_update_tests {
    use wafer_block::capabilities::BlockCapabilities;

    use super::*;

    /// Verify that `runtime_capabilities_mut` (via the Block trait) atomically
    /// replaces the internal capabilities and that subsequent calls to
    /// `block_capabilities()` reflect the new set.
    ///
    /// Loading a real WASM module is required to construct a WasmiBlock.
    /// We use the minimal WAT fixture from the fuel-exhaustion test — it has all
    /// required exports and exercises no guest logic.
    #[test]
    fn runtime_capabilities_mut_replaces_caps() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "__wafer_alloc") (param i32) (result i32) i32.const 0)
              (func (export "__wafer_info") (result i64) i64.const 0)
              (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const 0)
              (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
            )
            "#,
        )
        .expect("WAT should parse");

        // Load with unrestricted capabilities.
        let block =
            WasmiBlock::load_with_capabilities(&wasm_bytes, BlockCapabilities::unrestricted())
                .expect("minimal WAT module should load");

        // Confirm initial state: unrestricted → network = true.
        let before = block
            .block_capabilities()
            .expect("WasmiBlock must return Some(caps)");
        assert!(before.network, "initial caps should have network=true");

        // Apply a narrower capability set via the Block trait method.
        let narrowed = BlockCapabilities::none();
        use wafer_block::Block;
        block.runtime_capabilities_mut(narrowed);

        // Confirm the update is visible.
        let after = block
            .block_capabilities()
            .expect("WasmiBlock must return Some(caps) after update");
        assert!(
            !after.network,
            "after runtime_capabilities_mut, network should be false"
        );
        assert!(
            !after.crypto,
            "after runtime_capabilities_mut, crypto should be false"
        );
        assert!(
            !after.raw_sql,
            "after runtime_capabilities_mut, raw_sql should be false"
        );
    }

    /// A guest that imports `wasi_snapshot_preview1::sched_yield` (pulled in by
    /// pure-Rust crates whose spin/back-off paths reference
    /// `std::thread::yield_now`, e.g. `scraper`/`ahash`) must link and
    /// instantiate. Before the `sched_yield` stub existed, `build_linker`
    /// provided no definition and instantiation failed with
    /// "cannot find definition for import wasi_snapshot_preview1::sched_yield".
    #[test]
    fn guest_importing_sched_yield_instantiates() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (import "wasi_snapshot_preview1" "sched_yield" (func $sched_yield (result i32)))
              (memory (export "memory") 1)
              (func (export "__wafer_alloc") (param i32) (result i32) i32.const 0)
              (func (export "__wafer_info") (result i64)
                (drop (call $sched_yield))
                i64.const 0)
              (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const 0)
              (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
            )
            "#,
        )
        .expect("WAT should parse");

        let block =
            WasmiBlock::load_with_capabilities(&wasm_bytes, BlockCapabilities::unrestricted())
                .expect("module importing sched_yield should compile");

        // instantiate() runs through build_linker — a missing sched_yield
        // definition would error here.
        let (mut store, instance) = instantiate(
            &block.engine,
            &block.linker,
            &block.module,
            &BlockCapabilities::unrestricted(),
            block.limits,
        )
        .expect("module importing sched_yield should instantiate");

        // Call a guest function that invokes sched_yield to prove the stub
        // returns success (errno 0) and the call completes.
        let info_fn = instance
            .get_typed_func::<(), i64>(&store, "__wafer_info")
            .expect("__wafer_info export");
        info_fn
            .call(&mut store, ())
            .expect("calling a guest fn that uses sched_yield should succeed");
    }
}
