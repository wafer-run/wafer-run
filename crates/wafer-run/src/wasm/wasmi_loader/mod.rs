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

use super::{capabilities::BlockCapabilities, stream::StreamRegistry};
use crate::{
    context::Context,
    runtime::wasm_state::{FuelLimit, ResourceLimits},
};

mod abi;
mod codec;
mod imports;
mod instance;
mod meta;
mod pool;
mod transcode;

use abi::*;
use codec::{abi_codec_of, verify_abi_version, AbiCodec, HostCodec};
use imports::*;
use instance::{apply_fuel, instantiate, ContextScope};
use meta::*;
// `wasm_pooling_host_override` is called from `runtime::seal` via the
// `wasmi_loader::` path, so it is re-exported (not a plain private `use`).
pub(crate) use pool::wasm_pooling_host_override;
// Host-facing pooling kill-switch env-var name, re-exported for `wasm::mod`
// (and, through it, external embedders).
pub use pool::WASM_POOLING_ENV;
use pool::{PooledInstance, MAX_CALLS_PER_INSTANCE, MAX_POOLED_INSTANCES};
use wafer_block::abi::GuestAction;

// ---------------------------------------------------------------------------
// WasmiBlock
// ---------------------------------------------------------------------------

/// `Block` implementation that runs a WASM module via the `wasmi` interpreter.
pub struct WasmiBlock {
    engine: Engine,
    module: Module,
    linker: Linker<WasmiHostState>,
    info_cache: Mutex<Option<BlockInfo>>,
    /// The capabilities every host call is checked against. Interior-mutable
    /// so `seal()` can install the effective set without reloading the WASM
    /// module. Reads are lock-free on uncontended RwLock; the write path is
    /// exercised at most once per block lifetime (startup).
    capabilities: parking_lot::RwLock<BlockCapabilities>,
    /// The upper bound the embedder loaded this block with, if it gave one
    /// (the `load_with_*` constructors). Fixed at load: every set installed
    /// through `runtime_capabilities_mut` is intersected with it, so the
    /// guest's own `BlockInfo::capabilities` declaration can narrow it but
    /// never widen it. `None` leaves the bound to `seal()`, which takes the
    /// operator's statement (`capabilities` block config), `none()` when
    /// there is none.
    bound: Option<BlockCapabilities>,
    /// Warn-once flag for outbound stripped headers.
    warned_outbound: std::sync::atomic::AtomicBool,
    /// Warn-once flag for inbound stripped headers.
    warned_inbound: std::sync::atomic::AtomicBool,
    /// Warn-once flag for a guest attempting to set host-owned identity
    /// (`auth.*`) — SEC-01.
    warned_forged_identity: std::sync::atomic::AtomicBool,
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
    /// Warm instance pool (PERF-01 Part B). Only the `handle` path checks
    /// instances out/in, and only when [`is_poolable`](Self::is_poolable)
    /// says the block opted into reuse; `info()`/`lifecycle()` always
    /// instantiate fresh. Checkout grants exclusive ownership (a `Store` is
    /// single-caller by construction), so concurrent calls get distinct
    /// instances.
    pool: parking_lot::Mutex<Vec<PooledInstance>>,
    /// Host-level kill switch, resolved once at load from
    /// [`WASM_POOLING_ENV`]. `false` forces every call cold regardless of
    /// the block's declared `InstanceMode`.
    pooling_enabled: bool,
    /// Cached policy decision: does this block's declared
    /// [`InstanceMode`](wafer_block::InstanceMode) opt into reuse
    /// (`Singleton` / `PerFlow`)? Pinned on first use *after* `info()`
    /// succeeds, so a transient `info()` failure cannot permanently pin the
    /// block cold.
    poolable: std::sync::OnceLock<bool>,
}

// Safety: Engine, Module, Linker are Send+Sync in wasmi 0.44. The Mutexes
// guard the info cache and the instance pool; `Store<WasmiHostState>` is
// compiler-verified `Send` elsewhere (per-call stores already cross await
// points inside `Send` handle futures), so parking pooled stores behind the
// Mutex is sound.
unsafe impl Send for WasmiBlock {}
unsafe impl Sync for WasmiBlock {}

/// A fresh engine whose `consume_fuel` flag matches `limits.fuel`.
fn engine_for(limits: ResourceLimits) -> Engine {
    let mut config = wasmi::Config::default();
    config.consume_fuel(limits.fuel.consume_fuel());
    Engine::new(&config)
}

impl WasmiBlock {
    /// Read a WASM module from disk and compile it (native-only convenience wrapper).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(path: &str) -> Result<Self, RuntimeError> {
        let bytes = std::fs::read(path)
            .map_err(|e| RuntimeError::Wasm(format!("reading WASM file: {e}")))?;
        Self::load_from_bytes(&bytes)
    }

    /// Compile a WASM module from raw bytes with no embedder capability bound
    /// and the default per-call resource limits ([`ResourceLimits::default`]:
    /// 100M fuel, 256-page / 16 MiB memory).
    ///
    /// The guest's own `BlockInfo::capabilities` declaration is a request,
    /// never a grant. Without an embedder bound, the guest runs with
    /// [`BlockCapabilities::none`] until [`Wafer::seal`](crate::Wafer::seal),
    /// which bounds it by what the operator stated in the block's
    /// `capabilities` config — every field left out is denied, so with no
    /// statement it keeps `none()` — and installs that ∩ the declaration.
    /// An embedder states the bound itself with
    /// [`load_with_capabilities`](Self::load_with_capabilities), or approves
    /// exactly what the guest declares with
    /// [`load_approving_declaration`](Self::load_approving_declaration).
    pub fn load_from_bytes(wasm_bytes: &[u8]) -> Result<Self, RuntimeError> {
        Self::load_from_bytes_with_limits(wasm_bytes, ResourceLimits::default())
    }

    /// Compile a WASM module from raw bytes with no embedder capability bound
    /// (see [`load_from_bytes`](Self::load_from_bytes)) and an explicit
    /// per-call [`FuelLimit`] (default memory cap).
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

    /// Compile a WASM module from raw bytes with no embedder capability bound
    /// (see [`load_from_bytes`](Self::load_from_bytes)) and explicit per-call
    /// [`ResourceLimits`] (fuel budget + linear-memory page cap).
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
        Self::compile(
            &engine_for(limits),
            wasm_bytes,
            BlockCapabilities::none(),
            None,
            limits,
        )
    }

    /// Compile a WASM module under the capability set `caps` and the default
    /// per-call resource limits ([`ResourceLimits::default`]).
    ///
    /// `caps` is the guest's upper bound for its whole lifetime: it is what
    /// the guest runs under until [`Wafer::seal`](crate::Wafer::seal), and
    /// `seal()` narrows it to `caps ∩ declared ∩ config`, where `declared` is
    /// the guest's own `BlockInfo::capabilities`. A guest cannot declare its
    /// way past `caps`.
    pub fn load_with_capabilities(
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
    ) -> Result<Self, RuntimeError> {
        Self::load_with_capabilities_and_limits(wasm_bytes, caps, ResourceLimits::default())
    }

    /// Compile a WASM module under the capability bound `caps` (see
    /// [`load_with_capabilities`](Self::load_with_capabilities)) and explicit
    /// per-call [`ResourceLimits`]. Creates a fresh engine whose
    /// `consume_fuel` flag matches the requested fuel mode.
    pub fn load_with_capabilities_and_limits(
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
        limits: ResourceLimits,
    ) -> Result<Self, RuntimeError> {
        Self::load_with_engine_and_limits(&engine_for(limits), wasm_bytes, caps, limits)
    }

    /// Compile a WASM module under the capability bound `caps` (see
    /// [`load_with_capabilities`](Self::load_with_capabilities)), reusing an
    /// existing `wasmi::Engine` (lets callers share fuel config), with the
    /// default per-call resource limits ([`ResourceLimits::default`]).
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

    /// Compile a WASM module under the capability bound `caps` (see
    /// [`load_with_capabilities`](Self::load_with_capabilities)), reusing an
    /// existing `wasmi::Engine`, with explicit per-call [`ResourceLimits`].
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
        Self::compile(engine, wasm_bytes, caps.clone(), Some(caps), limits)
    }

    /// Compile a WASM module whose embedder approves exactly the capabilities
    /// the guest declares in its `BlockInfo::capabilities` (`none()` when it
    /// declares none): the declaration becomes the bound, as if passed to
    /// [`load_with_capabilities_and_limits`](Self::load_with_capabilities_and_limits).
    ///
    /// This trusts the guest's request, so it is for guests the embedder
    /// has vetted or built itself — never for untrusted uploads, which must
    /// be loaded with a bound the embedder chose.
    pub fn load_approving_declaration(
        wasm_bytes: &[u8],
        limits: ResourceLimits,
    ) -> Result<Self, RuntimeError> {
        let mut block = Self::compile(
            &engine_for(limits),
            wasm_bytes,
            BlockCapabilities::none(),
            None,
            limits,
        )?;
        let declared = block
            .info()
            .capabilities
            .unwrap_or_else(BlockCapabilities::none);
        *block.capabilities.get_mut() = declared.clone();
        block.bound = Some(declared);
        Ok(block)
    }

    /// The one constructor body: `caps` is what the guest runs under until
    /// `seal()`, `bound` what `runtime_capabilities_mut` intersects with.
    fn compile(
        engine: &Engine,
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
        bound: Option<BlockCapabilities>,
        limits: ResourceLimits,
    ) -> Result<Self, RuntimeError> {
        let module = Module::new(engine, wasm_bytes)
            .map_err(|e| RuntimeError::Wasm(format!("compiling WASM module: {e}")))?;
        let linker = build_linker(engine)?;
        // Fail loud at load on a mistyped kill-switch value — never silently
        // fall back to pooled or cold (config rule; see WASM_POOLING_ENV).
        let pooling_enabled = wasm_pooling_host_override()?;
        Ok(Self {
            engine: engine.clone(),
            module,
            linker,
            info_cache: Mutex::new(None),
            capabilities: parking_lot::RwLock::new(caps),
            bound,
            warned_outbound: std::sync::atomic::AtomicBool::new(false),
            warned_inbound: std::sync::atomic::AtomicBool::new(false),
            warned_forged_identity: std::sync::atomic::AtomicBool::new(false),
            asset_loader: parking_lot::RwLock::new(Arc::new(crate::asset_loader::NoopAssetLoader)),
            limits,
            pool: parking_lot::Mutex::new(Vec::new()),
            pooling_enabled,
            poolable: std::sync::OnceLock::new(),
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

    /// Build a fresh store + instance from this block's engine/linker/module,
    /// exercising the same [`instantiate`] path used at call time. Used by
    /// tests that need to inspect post-instantiation host state (e.g.
    /// negotiated [`HostCodec`]) without going through `handle`/`info`.
    #[cfg(test)]
    fn instantiate_for_test(
        &self,
    ) -> Result<(Store<WasmiHostState>, wasmi::Instance), RuntimeError> {
        instantiate(
            &self.engine,
            &self.linker,
            &self.module,
            &self.capabilities.read(),
            self.limits,
        )
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
        // The guest ABI hands the guest the whole body, so a body that did
        // not arrive whole never reaches it.
        let body = match input.collect_to_bytes().await {
            Ok(body) => body,
            Err(e) => return OutputStream::error(e),
        };

        // Withhold the sensitive headers the guest may not read, and capture
        // what the host owns for this frame: the identity (`auth.*`, SEC-01)
        // established upstream — which the guest sees but cannot alter — and
        // the inbound sensitive headers. Both are restored on every guest
        // egress — Respond, Error, Continue, and nested `call_block` (seeded
        // into the store below for the streaming-ABI path).
        let (msg, host_owned) = {
            let inbound = prepare_guest_inbound(msg.meta, &self.capabilities.read());
            if !inbound.withheld.is_empty() {
                self.warn_once_stripped_inbound(&inbound.withheld);
            }
            (
                Message {
                    meta: inbound.meta,
                    ..msg
                },
                inbound.host_owned,
            )
        };

        let codec = abi_codec_of(&self.module);
        let msg_bytes = {
            let frame = wafer_block::abi::CallFrameRef(&msg, &body);
            let encoded = match codec {
                AbiCodec::V1Json => serde_json::to_vec(&frame).map_err(|e| e.to_string()),
                AbiCodec::V2Rmp => wafer_block::codec::encode(&frame).map_err(|e| e.to_string()),
            };
            match encoded {
                Ok(b) => b,
                Err(e) => {
                    return OutputStream::error(WaferError::new(
                        ErrorCode::Internal,
                        format!("serializing message: {e}"),
                    ));
                }
            }
        };

        let (result_bytes, lease) = match self
            .call_handle_guest(ctx, attachments, host_owned.clone(), codec, &msg_bytes)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::Internal,
                    format!("WASM handle error: {e}"),
                ));
            }
        };

        // Decode the guest's result with the negotiated codec (the shared
        // `wafer_block::abi::GuestResult` type — same one the SDK glue
        // serializes, so the two sides cannot drift) and map it back to an
        // OutputStream.
        let parsed: Result<wafer_block::abi::GuestResult, String> = match codec {
            AbiCodec::V1Json => serde_json::from_slice(&result_bytes).map_err(|e| e.to_string()),
            AbiCodec::V2Rmp => wafer_block::codec::decode(&result_bytes).map_err(|e| e.to_string()),
        };

        // Pool checkin — clean-exit path only. A trap / fuel exhaustion /
        // resume error never reaches this point (the `Err` arm above returned
        // early and dropped the instance). Here the remaining gate is the
        // result decode: only a well-formed `GuestResult` with a known action
        // proves the guest ran its ABI glue to completion, so anything else
        // drops the instance too (failure replacement — the next call
        // instantiates fresh). A well-formed `action == "Error"` is a clean
        // exit: the guest handled an application error and returned normally.
        if let Some(leased) = lease {
            match &parsed {
                // A successful decode is, by construction, a well-formed
                // `GuestResult` with a known action (`GuestAction` rejects any
                // other discriminator at decode time) — proof the guest ran
                // its ABI glue to completion, so check the instance back in.
                Ok(_) => self.checkin(leased),
                Err(_) => drop(leased),
            }
        }

        // Every guest egress goes through the same header allowlist and
        // identity restore (`sanitize_guest_egress`).
        let egress = |meta: Vec<MetaEntry>, kind: GuestEgress| {
            self.sanitize_egress(meta, &self.capabilities.read(), &host_owned, kind)
        };
        match parsed {
            Ok(result) => match result.action {
                GuestAction::Respond => {
                    let (data, meta) = result
                        .response
                        .map(|r| (r.data, egress(r.meta, GuestEgress::Respond)))
                        .unwrap_or_default();
                    if meta.is_empty() {
                        OutputStream::respond(data)
                    } else {
                        OutputStream::respond_with_meta(data, meta)
                    }
                }
                GuestAction::Error => {
                    let mut e = result.error.unwrap_or_else(|| {
                        WaferError::new(
                            ErrorCode::Internal,
                            "WASM block returned error with no details",
                        )
                    });
                    e.meta = egress(e.meta, GuestEgress::Error);
                    OutputStream::error(e)
                }
                GuestAction::Drop => OutputStream::drop_request(),
                GuestAction::Continue => {
                    let mut msg = result.message.unwrap_or_else(|| Message::new("continue"));
                    msg.meta = egress(msg.meta, GuestEgress::Continue);
                    OutputStream::continue_with(msg)
                }
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

    /// Is this block eligible for warm instance pooling?
    ///
    /// Policy: the host kill switch ([`WASM_POOLING_ENV`], resolved at load)
    /// must permit pooling, AND the block's declared
    /// [`InstanceMode`](wafer_block::InstanceMode) must be a state-retaining
    /// one (`Singleton` / `PerFlow` — "my state may live across calls").
    /// `PerNode` (the undeclared default) and `PerExecution` keep today's
    /// fresh-instance-per-call behavior.
    fn is_poolable(&self) -> bool {
        if !self.pooling_enabled {
            return false;
        }
        if let Some(decided) = self.poolable.get() {
            return *decided;
        }
        let mode = self.info().instance_mode;
        let poolable = matches!(
            mode,
            wafer_block::InstanceMode::Singleton | wafer_block::InstanceMode::PerFlow
        );
        // Pin the decision only once `info()` has actually succeeded (the
        // cache is populated). A failed `info()` reports the placeholder
        // BlockInfo (default `PerNode`), and that transient fault must not
        // pin the block cold forever.
        if self.info_cache.lock().is_ok_and(|guard| guard.is_some()) {
            let _ = self.poolable.set(poolable);
        }
        poolable
    }

    /// Number of idle instances currently retained in the warm pool.
    ///
    /// Observability/test accessor — the pool is otherwise invisible from
    /// the outside. Always `0` for blocks that did not opt into reuse.
    pub fn pooled_instance_count(&self) -> usize {
        self.pool.lock().len()
    }

    /// Check an instance out of the warm pool, or instantiate fresh when the
    /// pool is empty. Checkout grants exclusive ownership; a pooled instance
    /// gets its fuel refilled (same budget as a fresh instantiation) and its
    /// capabilities snapshot refreshed so a set narrowed after the instance
    /// was created (e.g. by `seal()`'s effective-capability propagation) can
    /// never be widened back by reuse.
    fn checkout(&self) -> Result<PooledInstance, RuntimeError> {
        let popped = self.pool.lock().pop();
        if let Some(mut leased) = popped {
            apply_fuel(
                &mut leased.store,
                self.limits.fuel,
                "refilling fuel at pool checkout",
            )?;
            leased.store.data_mut().capabilities = self.capabilities.read().clone();
            return Ok(leased);
        }
        let caps_snapshot = self.capabilities.read().clone();
        let (store, instance) = instantiate(
            &self.engine,
            &self.linker,
            &self.module,
            &caps_snapshot,
            self.limits,
        )?;
        Ok(PooledInstance {
            store,
            instance,
            calls_served: 0,
        })
    }

    /// Return an instance to the warm pool after a clean exit.
    ///
    /// Recycles (drops) the instance instead when it has served
    /// [`MAX_CALLS_PER_INSTANCE`] calls or its linear memory has grown to the
    /// block's page cap — the core ABI has no guest-side free for
    /// host-written buffers, so reused instances leak guest heap per call and
    /// must be replaced periodically. Otherwise resets all per-call host
    /// state: the context slot (already cleared by `ContextScope::drop`,
    /// re-cleared here as hygiene), attachments, the SEC-01 protected-meta
    /// snapshot, every `pending_*` resume slot, and the `StreamRegistry` —
    /// replacing the registry drops any handle the guest leaked, which
    /// cancels in-flight response streams via their paired
    /// `CancellationToken`s exactly as a store drop would. Linear memory and
    /// globals are deliberately NOT reset: that is the declared-reuse
    /// semantics the block opted into.
    fn checkin(&self, mut leased: PooledInstance) {
        leased.calls_served = leased.calls_served.saturating_add(1);
        if leased.calls_served >= MAX_CALLS_PER_INSTANCE {
            return;
        }
        let Some(memory) = leased.instance.get_memory(&leased.store, "memory") else {
            return;
        };
        let cap_bytes = (self.limits.memory_pages as usize).saturating_mul(65536);
        if memory.data_size(&leased.store) >= cap_bytes {
            return;
        }

        let data = leased.store.data_mut();
        data.context = None;
        data.current_attachments = None;
        data.host_owned_meta = HostOwnedMeta::default();
        data.pending_stream_finish = None;
        data.pending_stream_read = None;
        data.pending_stream_take_error = None;
        data.pending_load_asset = None;
        data.streams =
            StreamRegistry::with_limits(self.limits.max_host_bytes, self.limits.max_live_streams);

        let mut pool = self.pool.lock();
        if pool.len() < MAX_POOLED_INSTANCES {
            pool.push(leased);
        }
        // Beyond the cap: drop on checkin (never queue).
    }

    /// Drive one `__wafer_handle` invocation, pool-aware.
    ///
    /// Pool-eligible blocks check an instance out (reusing a warm one when
    /// available) and, on a clean transport-level exit, hand it back to the
    /// caller as a lease — `handle_inner` checks it in only after the result
    /// decodes as a well-formed `GuestResult`. Any error path drops the
    /// instance. Cold blocks instantiate fresh and return no lease, exactly
    /// today's behavior.
    async fn call_handle_guest(
        &self,
        ctx: &dyn Context,
        attachments: Option<std::collections::BTreeMap<String, wafer_block::Attachment>>,
        host_owned: HostOwnedMeta,
        codec: AbiCodec,
        msg_bytes: &[u8],
    ) -> Result<(Vec<u8>, Option<PooledInstance>), RuntimeError> {
        let setup = |store: &mut Store<WasmiHostState>, instance: wasmi::Instance| {
            let alloc_fn = instance
                .get_typed_func::<i32, i32>(&*store, "__wafer_alloc")
                .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_alloc: {e}")))?;
            let handle_fn = instance
                .get_typed_func::<(i32, i32), i64>(&*store, "__wafer_handle")
                .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_handle: {e}")))?;
            let memory = instance
                .get_memory(&*store, "memory")
                .ok_or_else(|| RuntimeError::Wasm("guest has no exported memory".to_string()))?;

            if codec == AbiCodec::V2Rmp {
                verify_abi_version(store, instance)?;
            }
            let ptr = write_guest_bytes(store, alloc_fn, memory, msg_bytes)?;
            let len = msg_bytes.len() as i32;
            Ok((handle_fn, ptr as i32, len))
        };

        if self.is_poolable() {
            let mut leased = self.checkout()?;
            let instance = leased.instance;
            let bytes = self
                .run_guest_call(
                    &mut leased.store,
                    instance,
                    ctx,
                    attachments,
                    host_owned,
                    setup,
                )
                .await?;
            Ok((bytes, Some(leased)))
        } else {
            let caps_snapshot = self.capabilities.read().clone();
            let (mut store, instance) = instantiate(
                &self.engine,
                &self.linker,
                &self.module,
                &caps_snapshot,
                self.limits,
            )?;
            let bytes = self
                .run_guest_call(&mut store, instance, ctx, attachments, host_owned, setup)
                .await?;
            Ok((bytes, None))
        }
    }

    /// Call a guest function that returns a packed i64 (ptr << 32 | len),
    /// handling the call_block trap+resume loop, on a fresh (never pooled)
    /// instance. Used by `lifecycle()` — `handle` goes through the
    /// pool-aware [`call_handle_guest`](Self::call_handle_guest) instead.
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
        let caps_snapshot = self.capabilities.read().clone();
        let (mut store, instance) = instantiate(
            &self.engine,
            &self.linker,
            &self.module,
            &caps_snapshot,
            self.limits,
        )?;
        // No inbound request for this path (e.g. `__wafer_lifecycle`): no
        // host-owned meta and no attachments.
        self.run_guest_call(
            &mut store,
            instance,
            ctx,
            None,
            HostOwnedMeta::default(),
            setup,
        )
        .await
    }

    /// Drive one guest invocation on the given store + instance: install the
    /// per-call context scope, run `setup`, and pump the trap+resume loop to
    /// completion. Shared by the cold path
    /// ([`call_guest_resumable`](Self::call_guest_resumable)) and the
    /// pool-aware handle path ([`call_handle_guest`](Self::call_handle_guest));
    /// ownership of the store stays with the caller so the pooled path can
    /// retain it after a clean exit.
    async fn run_guest_call(
        &self,
        store: &mut Store<WasmiHostState>,
        instance: wasmi::Instance,
        ctx: &dyn Context,
        attachments: Option<std::collections::BTreeMap<String, wafer_block::Attachment>>,
        host_owned: HostOwnedMeta,
        setup: impl FnOnce(
            &mut Store<WasmiHostState>,
            wasmi::Instance,
        )
            -> Result<(wasmi::TypedFunc<(i32, i32), i64>, i32, i32), RuntimeError>,
    ) -> Result<Vec<u8>, RuntimeError> {
        // Install an owned clone of the context (and inbound attachments)
        // for the duration of this call. `ContextScope::drop` clears the
        // store's `context` slot on *every* exit path — `?`, early
        // `return Err`, the unhandled-trap branch, or success — so a stale
        // context never leaks into a later invocation. From here on the
        // store is reached through `scope`.
        let mut scope = ContextScope::new(store, ctx, attachments, host_owned);

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
                    // The guest's nested-call message is a guest egress: it
                    // gets the header allowlist, and the host-owned identity
                    // is restored on it so the guest cannot forge identity
                    // for the callee (SEC-01).
                    let state = data.streams.get_mut(handle);
                    state.map(|s| {
                        let req = s.take_finish_request();
                        let atts = s.take_attachments();
                        (req, atts)
                    })
                };
                let resume_code: i32 = match take_result {
                    Some((Ok((target, mut msg, body)), attachments)) => {
                        let data = scope.store().data();
                        msg.meta = self.sanitize_egress(
                            msg.meta,
                            &data.capabilities,
                            &data.host_owned_meta,
                            GuestEgress::Call,
                        );
                        // A JSON-codec guest writes its request body as JSON;
                        // the callee's wire DTOs are MessagePack. Transcode at
                        // the boundary so both sides keep their own codec.
                        let json = scope.store().data().host_codec == HostCodec::Json;
                        let body: Result<Vec<u8>, WaferError> = if json && !body.is_empty() {
                            transcode::json_to_rmp(&body)
                        } else {
                            Ok(body)
                        };
                        match body {
                            Err(e) => {
                                // The guest sent a body its declared codec
                                // cannot carry. Record it so take_error
                                // explains, and resume with the negative code
                                // like any other dispatch failure.
                                let code = e.code;
                                if let Some(state) =
                                    scope.store_mut().data_mut().streams.get_mut(handle)
                                {
                                    state.record_error_and_close(e);
                                }
                                error_code_to_neg_i32(code)
                            }
                            Ok(body) => {
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
                                    ctx.call_block_with_attachments(
                                        &target,
                                        msg,
                                        input,
                                        attachments,
                                    )
                                    .await
                                };
                                if let Some(state) =
                                    scope.store_mut().data_mut().streams.get_mut(handle)
                                {
                                    state.finish_with_stream(out);
                                }
                                0
                            }
                        }
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
                let json = scope.store().data().host_codec == HostCodec::Json;
                // `raw_frames` is read AFTER the pull, because the marker that
                // sets it arrives on the same `next_chunk` call that yields the
                // first raw frame (Meta events are skipped as data).
                let (next, raw_frames) = match scope.store_mut().data_mut().streams.get_mut(handle)
                {
                    Some(s) => {
                        let next = s.next_chunk().await;
                        (next, s.raw_frames())
                    }
                    None => (
                        Err(WaferError::new(
                            ErrorCode::NotFound,
                            "unknown stream handle",
                        )),
                        false,
                    ),
                };
                // The callee answers in MessagePack; a JSON-codec guest can
                // only read JSON. Transcode each frame at the boundary, with
                // two exclusions:
                //  - an *empty* frame carries no value to transcode
                //    (`ok_empty()` sends `Chunk(vec![])`), so it passes through
                //    untouched rather than failing as malformed MessagePack;
                //  - a frame the producer marked raw
                //    (`wafer_block::stream::FRAME_ENCODING_META`) is
                //    application bytes, not a wire DTO — `storage.get`'s object
                //    body, `network.do`'s response body — and is forwarded
                //    verbatim. The typed header frame ahead of the marker is
                //    still transcoded.
                let next = match next {
                    Ok(Some(bytes)) if json && !bytes.is_empty() && !raw_frames => {
                        match transcode::rmp_to_json(&bytes) {
                            Ok(b) => Ok(Some(b)),
                            Err(e) => {
                                // A frame the callee wrote is not a wire DTO.
                                // Fail the stream rather than hand the guest
                                // bytes it cannot read.
                                if let Some(s) =
                                    scope.store_mut().data_mut().streams.get_mut(handle)
                                {
                                    s.record_error_and_close(e.clone());
                                }
                                Err(e)
                            }
                        }
                    }
                    other => other,
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
                        // Encode in whichever codec the guest negotiated —
                        // an error it cannot decode is no error at all.
                        let bytes = match scope.store().data().host_codec {
                            HostCodec::Rmp => wafer_block::codec::encode(&err).map_err(|e| {
                                RuntimeError::Wasm(format!(
                                    "encoding WaferError for stream_take_error: {e}"
                                ))
                            })?,
                            HostCodec::Json => serde_json::to_vec(&err).map_err(|e| {
                                RuntimeError::Wasm(format!(
                                    "encoding WaferError as JSON for stream_take_error: {e}"
                                ))
                            })?,
                        };
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

    /// [`sanitize_guest_egress`] plus the warn-once reports for what it
    /// dropped.
    fn sanitize_egress(
        &self,
        meta: Vec<MetaEntry>,
        caps: &BlockCapabilities,
        host_owned: &HostOwnedMeta,
        egress: GuestEgress,
    ) -> Vec<MetaEntry> {
        let out = sanitize_guest_egress(meta, caps, host_owned, egress);
        if !out.stripped.is_empty() {
            self.warn_once_stripped_outbound(&out.stripped);
        }
        if !out.forged.is_empty() {
            self.warn_once_forged_identity(&out.forged);
        }
        out.meta
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

    fn warn_once_forged_identity(&self, keys: &[String]) {
        use std::sync::atomic::Ordering;
        if self.warned_forged_identity.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::warn!(
            block = %self.info().name,
            keys = ?keys,
            "guest attempted to set host-owned identity metadata (auth.*) — \
             ignored; host-provided identity preserved"
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

            let codec = abi_codec_of(&self.module);
            if codec == AbiCodec::V2Rmp {
                verify_abi_version(&mut store, instance)?;
            }

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
            let info: BlockInfo = match codec {
                AbiCodec::V1Json => serde_json::from_slice(&bytes)
                    .map_err(|e| RuntimeError::Wasm(format!("deserializing BlockInfo: {e}")))?,
                AbiCodec::V2Rmp => wafer_block::codec::decode(&bytes)
                    .map_err(|e| RuntimeError::Wasm(format!("deserializing BlockInfo: {e}")))?,
            };
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
                // is infallible, so we must return *something*: the
                // placeholder name "unknown", which is never a valid
                // registration name, so registering this block fails with
                // `RuntimeError::BlockNameMismatch` (reported "unknown"). Log
                // at `error!` so the cause is visible next to that refusal.
                // The failure is deliberately NOT cached: a transient
                // instantiation fault (e.g. fuel exhaustion) can recover on a
                // later call.
                error!(
                    "WasmiBlock::info() failed: {e}; reporting placeholder name \
                     \"unknown\", so registering this block will be refused"
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
        let codec = abi_codec_of(&self.module);
        let event_bytes = match codec {
            AbiCodec::V1Json => serde_json::to_vec(&event).map_err(|e| e.to_string()),
            AbiCodec::V2Rmp => wafer_block::codec::encode(&event).map_err(|e| e.to_string()),
        }
        .map_err(|e| {
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

                if codec == AbiCodec::V2Rmp {
                    verify_abi_version(store, instance)?;
                }
                let ptr = write_guest_bytes(store, alloc_fn, memory, &event_bytes)?;
                let len = event_bytes.len() as i32;
                Ok((lifecycle_fn, ptr as i32, len))
            })
            .await
            .map_err(|e| {
                WaferError::new(ErrorCode::Internal, format!("WASM lifecycle error: {e}"))
            })?;

        // The guest returns a codec-encoded Result<(), WaferError>.
        let result: std::result::Result<(), WaferError> = match codec {
            AbiCodec::V1Json => serde_json::from_slice(&result_bytes).map_err(|e| e.to_string()),
            AbiCodec::V2Rmp => wafer_block::codec::decode(&result_bytes).map_err(|e| e.to_string()),
        }
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

    fn capability_bound(&self) -> Option<BlockCapabilities> {
        self.bound.clone()
    }

    fn runtime_capabilities_mut(&self, new: BlockCapabilities) {
        let enforced = match &self.bound {
            Some(bound) => bound.intersect(&new),
            None => new,
        };
        *self.capabilities.write() = enforced;
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
        assert!(
            before.network.is_enabled(),
            "initial caps should have network enabled"
        );

        // Apply a narrower capability set via the Block trait method.
        let narrowed = BlockCapabilities::none();
        use wafer_block::Block;
        block.runtime_capabilities_mut(narrowed);

        // Confirm the update is visible.
        let after = block
            .block_capabilities()
            .expect("WasmiBlock must return Some(caps) after update");
        assert!(
            !after.network.is_enabled(),
            "after runtime_capabilities_mut, network should be disabled"
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

    fn minimal_guest() -> Vec<u8> {
        wat::parse_str(
            r#"(module
              (memory (export "memory") 1)
              (func (export "__wafer_info") (result i64) i64.const 0))"#,
        )
        .expect("WAT should parse")
    }

    /// The capabilities an embedder loads a block with are an upper bound:
    /// installing a wider set leaves the block at the bound.
    #[test]
    fn runtime_capabilities_mut_never_widens_past_the_load_bound() {
        use wafer_block::Block;
        let block = WasmiBlock::load_with_capabilities(&minimal_guest(), BlockCapabilities::none())
            .expect("minimal WAT module should load");

        let mut wider = BlockCapabilities::unrestricted();
        wider.headers.readable = vec!["authorization".to_string()];
        block.runtime_capabilities_mut(wider);

        assert_eq!(block.block_capabilities(), Some(BlockCapabilities::none()));
    }

    /// Without a load bound the block enforces the set installed as given
    /// (header opt-ins included): `seal()` has already bounded it by the
    /// operator's statement.
    #[test]
    fn runtime_capabilities_mut_installs_the_set_on_an_unbounded_block() {
        use wafer_block::Block;
        let block = WasmiBlock::load_from_bytes(&minimal_guest()).expect("loads");

        let mut declared = BlockCapabilities::none();
        declared.headers.readable = vec!["authorization".to_string()];
        block.runtime_capabilities_mut(declared.clone());

        assert_eq!(block.block_capabilities(), Some(declared));
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

    #[test]
    fn host_codec_defaults_to_rmp_without_the_export() {
        let wat = r#"(module
            (memory (export "memory") 1)
            (func (export "__wafer_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "__wafer_info") (result i64) i64.const 0)
            (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const 0)
        )"#;
        let wasm = wat::parse_str(wat).unwrap();
        let block =
            WasmiBlock::load_with_capabilities(&wasm, BlockCapabilities::unrestricted()).unwrap();
        let (store, _inst) = block.instantiate_for_test().unwrap();
        assert_eq!(store.data().host_codec, HostCodec::Rmp);
    }

    #[test]
    fn host_codec_json_is_negotiated_by_export() {
        let wat = r#"(module
            (memory (export "memory") 1)
            (func (export "__wafer_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "__wafer_info") (result i64) i64.const 0)
            (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const 0)
            (func (export "__wafer_host_codec") (result i32) i32.const 1)
        )"#;
        let wasm = wat::parse_str(wat).unwrap();
        let block =
            WasmiBlock::load_with_capabilities(&wasm, BlockCapabilities::unrestricted()).unwrap();
        let (store, _inst) = block.instantiate_for_test().unwrap();
        assert_eq!(store.data().host_codec, HostCodec::Json);
    }

    /// Codec negotiation CALLS a guest export, so it spends fuel from the
    /// budget the loader just set. The store must be refilled afterwards —
    /// otherwise the first real dispatch starts short by however much the
    /// guest's `__wafer_host_codec` happened to burn (an unbounded amount:
    /// the guest chooses the body).
    #[test]
    fn fuel_is_refilled_after_codec_negotiation() {
        // `__wafer_host_codec` burns a visible amount of fuel before
        // answering, so a missing refill is measurable rather than a
        // one-instruction rounding difference.
        let wat = r#"(module
            (memory (export "memory") 1)
            (func (export "__wafer_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "__wafer_info") (result i64) i64.const 0)
            (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const 0)
            (func (export "__wafer_host_codec") (result i32)
                (local $n i32)
                (local.set $n (i32.const 100000))
                (block $done
                    (loop $l
                        (br_if $done (i32.eqz (local.get $n)))
                        (local.set $n (i32.sub (local.get $n) (i32.const 1)))
                        (br $l)))
                (i32.const 1))
        )"#;
        let wasm = wat::parse_str(wat).unwrap();
        let block =
            WasmiBlock::load_with_capabilities(&wasm, BlockCapabilities::unrestricted()).unwrap();
        let (store, _inst) = block.instantiate_for_test().unwrap();
        assert_eq!(store.data().host_codec, HostCodec::Json);
        assert_eq!(
            store.get_fuel().expect("the default limits are metered"),
            crate::runtime::wasm_state::DEFAULT_FUEL,
            "the first guest call must start from a full fuel budget"
        );
    }

    // -----------------------------------------------------------------------
    // Host-call codec: attachments stay rmp-only
    // -----------------------------------------------------------------------

    /// The payload the attach probe hands to `__wafer_host_stream_attach`: a
    /// well-formed rmp `(id, Attachment)` tuple, so the only thing that can
    /// make the attach fail is the codec refusal itself.
    fn attach_probe_payload() -> Vec<u8> {
        wafer_block::codec::encode(&(
            "a".to_string(),
            wafer_block::Attachment {
                mime: "text/plain".to_string(),
                bytes: b"hi".to_vec(),
                filename: None,
            },
        ))
        .expect("encoding the attach payload")
    }

    /// A guest that opens a stream and immediately attaches to it, storing the
    /// attach status code at linear-memory offset 64. `__wafer_handle`'s two
    /// params carry the (ptr, len) of the attach payload the host wrote into
    /// guest memory, so the same WAT serves both codecs.
    fn attach_probe_module(host_codec_export: &str) -> Vec<u8> {
        let wat = format!(
            r#"(module
            (import "wafer" "__wafer_host_stream_init"
                (func $init (param i32 i32 i32 i32) (result i64)))
            (import "wafer" "__wafer_host_stream_attach"
                (func $attach (param i64 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "wafer-run/config")
            (data (i32.const 32) "{{\"kind\":\"config.get\",\"meta\":[]}}")
            (func (export "__wafer_alloc") (param i32) (result i32) i32.const 4096)
            (func (export "__wafer_info") (result i64) i64.const 0)
            {host_codec_export}
            (func (export "__wafer_handle") (param i32 i32) (result i64)
                (local $h i64)
                (local.set $h
                    (call $init (i32.const 0) (i32.const 16) (i32.const 32) (i32.const 31)))
                (i32.store (i32.const 64)
                    (call $attach (local.get $h) (local.get 0) (local.get 1)))
                i64.const 0)
        )"#
        );
        wat::parse_str(&wat).expect("WAT should parse")
    }

    /// Run the attach probe and return the status code the guest observed.
    fn run_attach_probe(wasm: &[u8]) -> i32 {
        let block = WasmiBlock::load_with_capabilities(wasm, BlockCapabilities::unrestricted())
            .expect("probe module should load");
        let (mut store, instance) = block
            .instantiate_for_test()
            .expect("probe module should instantiate");
        let memory = instance
            .get_memory(&store, "memory")
            .expect("probe module exports memory");
        let payload = attach_probe_payload();
        memory
            .write(&mut store, 128, &payload)
            .expect("writing the attach payload into guest memory");
        let handle_fn = instance
            .get_typed_func::<(i32, i32), i64>(&store, "__wafer_handle")
            .expect("__wafer_handle export");
        handle_fn
            .call(&mut store, (128, payload.len() as i32))
            .expect("the probe never traps");
        let mut status = [0u8; 4];
        memory
            .read(&store, 64, &mut status)
            .expect("reading the attach status back");
        i32::from_le_bytes(status)
    }

    #[test]
    fn json_guest_attach_is_refused() {
        // A JSON-codec guest has no MessagePack encoder, so an attach payload
        // could only be mis-decoded: the host refuses instead.
        assert_eq!(
            run_attach_probe(&attach_probe_module(
                r#"(func (export "__wafer_host_codec") (result i32) i32.const 1)"#
            )),
            error_code_to_neg_i32(ErrorCode::InvalidArgument),
            "a JSON-codec guest must be refused"
        );
        // The same module without the export negotiates rmp — attach works.
        assert_eq!(
            run_attach_probe(&attach_probe_module("")),
            0,
            "an rmp guest must be unaffected"
        );
    }

    /// A guest that looks up the inbound attachment `"a"` and stores the packed
    /// `i64` the host returned at linear-memory offset 64.
    fn lookup_probe_module(host_codec_export: &str) -> Vec<u8> {
        let wat = format!(
            r#"(module
            (import "wafer" "__wafer_host_lookup_attachment"
                (func $lookup (param i32 i32) (result i64)))
            (memory (export "memory") 1)
            (data (i32.const 0) "a")
            (func (export "__wafer_alloc") (param i32) (result i32) i32.const 4096)
            (func (export "__wafer_info") (result i64) i64.const 0)
            {host_codec_export}
            (func (export "__wafer_handle") (param i32 i32) (result i64)
                (i64.store (i32.const 64)
                    (call $lookup (i32.const 0) (i32.const 1)))
                i64.const 0)
        )"#
        );
        wat::parse_str(&wat).expect("WAT should parse")
    }

    #[test]
    fn json_guest_lookup_attachment_is_refused() {
        let att = wafer_block::Attachment {
            mime: "text/plain".to_string(),
            bytes: b"hi".to_vec(),
            filename: None,
        };

        let run = |wasm: &[u8]| -> i64 {
            let block = WasmiBlock::load_with_capabilities(wasm, BlockCapabilities::unrestricted())
                .expect("probe module should load");
            let (mut store, instance) = block
                .instantiate_for_test()
                .expect("probe module should instantiate");
            // Seed the call frame the way the runtime does before __wafer_handle.
            store.data_mut().current_attachments = Some(
                [("a".to_string(), att.clone())]
                    .into_iter()
                    .collect::<std::collections::BTreeMap<_, _>>(),
            );
            let memory = instance
                .get_memory(&store, "memory")
                .expect("probe module exports memory");
            instance
                .get_typed_func::<(i32, i32), i64>(&store, "__wafer_handle")
                .expect("__wafer_handle export")
                .call(&mut store, (0, 0))
                .expect("the probe never traps");
            let mut packed = [0u8; 8];
            memory
                .read(&store, 64, &mut packed)
                .expect("reading the lookup result back");
            i64::from_le_bytes(packed)
        };

        // Attachments are rmp-only: a JSON guest is refused even though the
        // attachment is present.
        assert_eq!(
            run(&lookup_probe_module(
                r#"(func (export "__wafer_host_codec") (result i32) i32.const 1)"#
            )),
            error_code_to_neg_i64(ErrorCode::InvalidArgument),
            "a JSON-codec guest must be refused"
        );
        // The same module without the export negotiates rmp and gets the
        // attachment, rmp-encoded, at the packed (ptr, len).
        let packed = run(&lookup_probe_module(""));
        assert!(packed > 0, "an rmp guest must get a packed pointer");
        assert_eq!(
            unpack_ptr_len(packed).expect("packed pointer").1 as usize,
            wafer_block::codec::encode(&att).unwrap().len(),
            "an rmp guest must get the whole encoded Attachment"
        );
    }

    /// Context whose `call_block` answers with an empty 200 — `ok_empty()`
    /// sends `StreamEvent::Chunk(vec![])`, the frame the read arm must pass
    /// through instead of transcoding.
    #[derive(Clone)]
    struct EmptyResponseContext;

    #[wafer_async_trait]
    impl Context for EmptyResponseContext {
        async fn call_block(
            &self,
            _name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            wafer_block::response::ok_empty()
        }

        fn is_cancelled(&self) -> bool {
            false
        }

        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }

        fn clone_arc(&self) -> Arc<dyn Context> {
            Arc::new(self.clone())
        }
        // Denies every access, as the trait's default `check_resource_access` does.
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_block::types::ResourceType,
            _access: wafer_block::types::ResourceAccess,
        ) -> bool {
            false
        }
    }

    /// An empty response frame must reach a JSON-codec guest as an empty frame,
    /// not as an `InvalidArgument` that kills the stream. Zero bytes are not
    /// malformed MessagePack — they are a successful empty body.
    #[tokio::test]
    async fn json_guest_reads_an_empty_response_frame() {
        let wat = r#"(module
            (import "wafer" "__wafer_host_stream_init"
                (func $init (param i32 i32 i32 i32) (result i64)))
            (import "wafer" "__wafer_host_stream_finish" (func $finish (param i64) (result i32)))
            (import "wafer" "__wafer_host_stream_read_chunk" (func $read (param i64) (result i64)))
            (memory (export "memory") 1)
            (data (i32.const 0) "wafer-run/config")
            (data (i32.const 32) "{\"kind\":\"config.get\",\"meta\":[]}")
            (func (export "__wafer_alloc") (param i32) (result i32) i32.const 4096)
            (func (export "__wafer_info") (result i64) i64.const 0)
            (func (export "__wafer_host_codec") (result i32) i32.const 1)
            (func (export "__wafer_handle") (param i32 i32) (result i64)
                (local $h i64)
                (local.set $h
                    (call $init (i32.const 0) (i32.const 16) (i32.const 32) (i32.const 31)))
                (i32.store (i32.const 64) (call $finish (local.get $h)))
                (i64.store (i32.const 72) (call $read (local.get $h)))
                i64.const 0)
        )"#;
        let wasm = wat::parse_str(wat).expect("WAT should parse");
        let block = WasmiBlock::load_with_capabilities(&wasm, BlockCapabilities::unrestricted())
            .expect("probe module should load");
        let (mut store, instance) = block
            .instantiate_for_test()
            .expect("probe module should instantiate");
        assert_eq!(store.data().host_codec, HostCodec::Json);

        // Drive the resume loop directly so the store — and with it the guest's
        // record of what the host returned — survives the call.
        block
            .run_guest_call(
                &mut store,
                instance,
                &EmptyResponseContext,
                None,
                HostOwnedMeta::default(),
                |store, instance| {
                    let f = instance
                        .get_typed_func::<(i32, i32), i64>(&*store, "__wafer_handle")
                        .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_handle: {e}")))?;
                    Ok((f, 0, 0))
                },
            )
            .await
            .expect("the guest call completes");

        let memory = instance
            .get_memory(&store, "memory")
            .expect("probe module exports memory");
        let mut finish = [0u8; 4];
        memory.read(&store, 64, &mut finish).expect("finish status");
        assert_eq!(i32::from_le_bytes(finish), 0, "stream_finish must succeed");

        let mut read = [0u8; 8];
        memory.read(&store, 72, &mut read).expect("read status");
        let packed = i64::from_le_bytes(read);
        assert!(
            packed >= 0,
            "an empty response frame must not fail the read (got {packed})"
        );
        assert_eq!(
            unpack_ptr_len(packed).expect("packed pointer").1,
            0,
            "the empty frame must arrive empty"
        );
    }

    #[test]
    fn host_codec_unknown_value_fails_instantiation() {
        let wat = r#"(module
            (memory (export "memory") 1)
            (func (export "__wafer_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "__wafer_info") (result i64) i64.const 0)
            (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const 0)
            (func (export "__wafer_host_codec") (result i32) i32.const 7)
        )"#;
        let wasm = wat::parse_str(wat).unwrap();
        let block =
            WasmiBlock::load_with_capabilities(&wasm, BlockCapabilities::unrestricted()).unwrap();
        let err = block.instantiate_for_test().err().expect("must refuse");
        assert!(err.to_string().contains("__wafer_host_codec"), "{err}");
    }
}
