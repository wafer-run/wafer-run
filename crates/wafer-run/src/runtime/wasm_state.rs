//! WASM runtime state: the shared fuel-metered engine (created lazily at
//! seal) and the host-injected asset loader propagated into WASM blocks.

use std::sync::Arc;

use wafer_block::error::RuntimeError;

use crate::asset_loader::{LoadAssetCallback, NoopAssetLoader};

/// Default per-guest-call fuel budget in wasmi fuel units. A single guest
/// invocation that consumes more than this traps with a fuel-exhaustion error.
///
/// This is the value used by [`FuelLimit::default`].
///
/// A fuel unit is wasmi's: since wasmi 2.0, one per executed Wasm operator
/// (`nop`, `drop` and the structural control operators are free) plus
/// per-byte costs for bulk memory and table operations. wasmi 0.44 charged
/// per instruction of its own internal bytecode, which fused several Wasm
/// operators into one, so the same guest work costs about 2–2.5× the fuel
/// it did under 0.44 (measured on the echo and TinyGo fixtures). The budget
/// was therefore raised from 0.44's 100M to 250M, keeping the headroom a
/// legitimate guest had. The cost is the worst case: a runaway guest's
/// tight loop burns one unit per operator, so the longest it can run per
/// call scales with the budget — about 70 ms at 100M, about 170 ms at 250M
/// (wasmi 2.0, native release build).
pub const DEFAULT_FUEL: u64 = 250_000_000;

/// Default per-guest-call WASM linear-memory cap, in 64 KiB wasmi pages
/// (256 pages = 16 MiB).
///
/// This is the value used by [`ResourceLimits::default`] and matches the
/// historical hard-coded cap (`MAX_WASM_MEMORY_PAGES`), so a guest's memory
/// growth is bounded identically to before unless a consumer raises the cap.
pub const DEFAULT_MAX_WASM_MEMORY_PAGES: u32 = 256;

/// Default per-guest-call aggregate cap on host-owned bytes copied *out* of
/// guest linear memory — each stream's target name and message, stream
/// request buffers, and attachment payloads (SEC-03). The wasmi linear-memory cap does not cover these host `Vec`s, so
/// without this bound a guest can copy the same small chunk repeatedly into
/// unbounded host memory. 256 MiB is generous for legitimate single-call
/// streaming while still bounding the amplification.
pub const DEFAULT_MAX_HOST_BYTES: usize = 256 * 1024 * 1024;

/// Default per-guest-call cap on concurrent live streams (SEC-03). Bounds the
/// host stream registry so a guest cannot grow it without limit by opening
/// handles it never closes.
pub const DEFAULT_MAX_LIVE_STREAMS: usize = 1024;

/// Default cap on WebAssembly table elements a guest may grow to (SEC-03).
/// The `ResourceLimiter` previously permitted unbounded table growth, which
/// the linear-memory cap does not constrain.
pub const DEFAULT_MAX_TABLE_ELEMENTS: u32 = 100_000;

/// Per-guest-call wasmi fuel budget for a [`Wafer`](super::Wafer) runtime.
///
/// Fuel metering bounds how much work a single WASM guest invocation may do
/// before it traps, protecting the host from runaway or adversarial guests.
/// Hosted, multi-tenant deployments want the bounded default; trusted
/// single-user embedders (e.g. gizza's native CLI and browser runtime) can
/// opt into [`FuelLimit::Unmetered`] so heavy tools — those bundling large
/// metadata that is deserialized on every call — run to completion.
///
/// Selected on the builder via
/// [`WaferBuilder::fuel_per_call`](crate::WaferBuilder::fuel_per_call) and
/// threaded into the wasmi loader. The default is
/// [`Metered(DEFAULT_FUEL)`](FuelLimit::Metered) — the bounded default cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuelLimit {
    /// Cap each guest invocation at `n` wasmi fuel units. The wasmi engine is
    /// configured with `consume_fuel(true)` and the store's fuel is set to `n`
    /// before the guest call (and refilled after a TinyGo `_start`).
    Metered(u64),
    /// Disable fuel metering entirely. The wasmi engine is configured with
    /// `consume_fuel(false)` and the store's fuel is never set — guests run
    /// without a per-call budget. Only safe for trusted, single-user runtimes.
    Unmetered,
}

impl Default for FuelLimit {
    fn default() -> Self {
        FuelLimit::Metered(DEFAULT_FUEL)
    }
}

#[cfg(feature = "wasmi")]
impl FuelLimit {
    /// Whether the wasmi engine must enable fuel consumption for this mode.
    ///
    /// `Metered` requires `consume_fuel(true)` so the store can set/track fuel;
    /// `Unmetered` requires `consume_fuel(false)` because wasmi rejects
    /// `set_fuel`/`get_fuel` when consumption is off, and the loader must not
    /// call them.
    pub(crate) fn consume_fuel(self) -> bool {
        matches!(self, FuelLimit::Metered(_))
    }
}

/// Maximum guest call depth (nested Wasm function frames) per guest call.
///
/// wasmi's own default moved from 1024 to 1000 in 1.0; the runtime pins the
/// value it has always enforced instead of inheriting whatever the
/// interpreter ships. Deeper recursion traps with a stack-overflow error, it
/// never overflows the host's native stack.
#[cfg(feature = "wasmi")]
pub(crate) const MAX_WASM_RECURSION_DEPTH: usize = 1024;

/// Maximum size of a guest call's value stack, in bytes (1 MiB). As
/// [`MAX_WASM_RECURSION_DEPTH`]: wasmi's default moved from 1 MiB to
/// 1,000,000 bytes in 1.0; the runtime pins the historical bound. Exceeding
/// it traps with a stack-overflow error.
#[cfg(feature = "wasmi")]
pub(crate) const MAX_WASM_STACK_BYTES: usize = 1024 * 1024;

/// How a runtime-created wasmi engine compiles a guest module's functions
/// into the bytecode it executes.
///
/// Selected per load through [`ResourceLimits::compilation`], or for a whole
/// runtime with
/// [`WaferBuilder::wasm_compilation`](crate::WaferBuilder::wasm_compilation).
/// Both modes validate the whole module when it is loaded, so a malformed
/// guest is rejected at load (at boot, for a runtime that loads its blocks
/// at startup) in either mode. They differ in when each function is
/// *translated*:
///
/// | mode | translation | load cost | per-call fuel |
/// |---|---|---|---|
/// | [`Eager`](Self::Eager) | every function, at load | highest | depends only on what the call executes |
/// | [`LazyTranslation`](Self::LazyTranslation) | each function on the first call that reaches it | about 3.5× (release) to 6× (debug) lower | not deterministic, so refused with metered fuel |
///
/// (Load-cost ratios measured on wasmi 2.0 over 1440 real tool modules.)
///
/// Because lazy translation bills a function's translation to the call that
/// first reaches it, a metered call's fuel use would depend on which
/// functions earlier calls — or a pooled predecessor instance — happened to
/// reach. [`LazyTranslation`](Self::LazyTranslation) is therefore only
/// accepted together with [`FuelLimit::Unmetered`]; every constructor that
/// takes the pair refuses `Metered` + `LazyTranslation` with
/// [`RuntimeError::Config`] instead of silently translating eagerly.
///
/// wasmi also offers a fully lazy mode that skips validation until a
/// function's first use. The runtime does not expose it: a malformed guest
/// would then load cleanly and fail mid-request, and the remaining load cost
/// under [`LazyTranslation`](Self::LazyTranslation) is validation, which is
/// what keeps that failure at load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum WasmCompilation {
    /// Translate every function when the module is loaded. Every compile
    /// cost is paid up front, and a metered call's fuel use is a function of
    /// what it executes alone. The default, and the only mode a
    /// [`FuelLimit::Metered`] runtime accepts — hosted, multi-tenant
    /// deployments keep deterministic fuel.
    #[default]
    Eager,
    /// Validate the whole module at load, but translate each function only
    /// when a call first reaches it. Loading is several times cheaper, which
    /// matters to a trusted single-user host that loads many modules per
    /// start and runs few of their functions (e.g. a CLI hosting hundreds of
    /// tools). The first call to reach a function pays for translating it.
    /// Requires [`FuelLimit::Unmetered`].
    LazyTranslation,
}

/// Refuse a compilation mode that would make metered fuel use depend on
/// call order (see [`WasmCompilation`]). The one check every constructor
/// that takes a fuel limit and a compilation mode runs.
pub(crate) fn check_compilation_mode(
    fuel: FuelLimit,
    compilation: WasmCompilation,
) -> Result<(), RuntimeError> {
    match (fuel, compilation) {
        (FuelLimit::Metered(_), WasmCompilation::LazyTranslation) => Err(RuntimeError::Config(
            "WasmCompilation::LazyTranslation requires FuelLimit::Unmetered: lazy translation \
             bills a function's translation to the first metered call that reaches it, so a \
             call's fuel use would depend on which calls ran before it. Use \
             WasmCompilation::Eager with a metered fuel limit."
                .to_string(),
        )),
        (FuelLimit::Metered(_), WasmCompilation::Eager)
        | (FuelLimit::Unmetered, WasmCompilation::Eager)
        | (FuelLimit::Unmetered, WasmCompilation::LazyTranslation) => Ok(()),
    }
}

/// The wasmi engine configuration every runtime-created engine uses.
///
/// Each setting that bounds a guest is stated here rather than inherited
/// from wasmi's defaults, which have changed across releases:
///
/// - `consume_fuel` follows `fuel` (see [`FuelLimit::consume_fuel`]).
/// - The call-depth and value-stack bounds are
///   [`MAX_WASM_RECURSION_DEPTH`] and [`MAX_WASM_STACK_BYTES`].
/// - The compilation mode is `compilation`, stated explicitly: wasmi's own
///   default has changed (Eager until 0.48, lazy translation since). A
///   metered engine always translates eagerly — `Metered` +
///   [`WasmCompilation::LazyTranslation`] is refused here (see
///   [`check_compilation_mode`]) — so a metered call's fuel use is a
///   function of what it executes, not of which functions earlier calls (or
///   a pooled predecessor) happened to reach.
#[cfg(feature = "wasmi")]
pub(crate) fn wasmi_config(
    fuel: FuelLimit,
    compilation: WasmCompilation,
) -> Result<wasmi::Config, RuntimeError> {
    check_compilation_mode(fuel, compilation)?;
    let mode = match compilation {
        WasmCompilation::Eager => wasmi::CompilationMode::Eager,
        WasmCompilation::LazyTranslation => wasmi::CompilationMode::LazyTranslation,
    };
    let mut config = wasmi::Config::default();
    config
        .consume_fuel(fuel.consume_fuel())
        .set_max_recursion_depth(MAX_WASM_RECURSION_DEPTH)
        .set_max_stack_height(MAX_WASM_STACK_BYTES)
        .compilation_mode(mode);
    Ok(config)
}

/// Per-guest-call resource limits applied to a directly-loaded [`WasmiBlock`].
///
/// Bundles the two independently-tunable per-call bounds — the wasmi
/// [`FuelLimit`] and the linear-memory page cap — so consumers that load WASM
/// blocks directly (e.g. gizza's native CLI and browser runtime) set both in a
/// single call via
/// [`WasmiBlock::load_from_bytes_with_limits`](crate::WasmiBlock::load_from_bytes_with_limits)
/// rather than threading two parallel arguments.
///
/// Hosted, multi-tenant deployments want the bounded [`default`](Self::default)
/// ([`DEFAULT_FUEL`] metered fuel, 256-page / 16 MiB memory). Trusted single-user
/// embedders raise either bound: [`FuelLimit::Unmetered`] for heavy compute,
/// or a larger `memory_pages` for memory-heavy tools (e.g. the `syntect`+font
/// code-screenshot tool needs ~24 MiB ≈ 384 pages). Omitting
/// `ResourceLimits` is the same as passing [`default`](Self::default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Per-guest-call wasmi fuel budget. See [`FuelLimit`].
    pub fuel: FuelLimit,
    /// Maximum WASM linear-memory size, in 64 KiB wasmi pages. A guest whose
    /// `memory.grow` would exceed this is denied (the grow returns -1).
    /// Defaults to [`DEFAULT_MAX_WASM_MEMORY_PAGES`] (256 pages = 16 MiB).
    pub memory_pages: u32,
    /// SEC-03: aggregate cap on host-owned bytes copied out of guest memory
    /// (stream-init name + message, stream request buffers, attachments) per
    /// guest call. Defaults to
    /// [`DEFAULT_MAX_HOST_BYTES`] (256 MiB).
    pub max_host_bytes: usize,
    /// SEC-03: cap on concurrent live streams per guest call. Defaults to
    /// [`DEFAULT_MAX_LIVE_STREAMS`] (1024).
    pub max_live_streams: usize,
    /// SEC-03: cap on WebAssembly table elements. Defaults to
    /// [`DEFAULT_MAX_TABLE_ELEMENTS`] (100_000).
    pub max_table_elements: u32,
    /// How the block's engine compiles the module. Defaults to
    /// [`WasmCompilation::Eager`]; [`WasmCompilation::LazyTranslation`] is
    /// accepted only with [`FuelLimit::Unmetered`], and a load that pairs it
    /// with `Metered` fuel fails.
    pub compilation: WasmCompilation,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            fuel: FuelLimit::default(),
            memory_pages: DEFAULT_MAX_WASM_MEMORY_PAGES,
            max_host_bytes: DEFAULT_MAX_HOST_BYTES,
            max_live_streams: DEFAULT_MAX_LIVE_STREAMS,
            max_table_elements: DEFAULT_MAX_TABLE_ELEMENTS,
            compilation: WasmCompilation::default(),
        }
    }
}

/// WASM-related runtime state, grouped out of the `Wafer` god-struct.
pub(crate) struct WasmState {
    /// Host-injected async loader for external wasm/js assets referenced by
    /// `BlockInfo::external_assets`. Defaults to `NoopAssetLoader`; hosts that
    /// need lazy asset loading call `Wafer::set_asset_loader` during startup.
    /// Propagated into each `WasmiBlock` at registration.
    pub(crate) asset_loader: Arc<dyn LoadAssetCallback>,
    /// Per-guest-call fuel budget applied to WASM blocks loaded through this
    /// runtime (the lazily-created shared engine + remote/lockfile blocks).
    /// Set via `WaferBuilder::fuel_per_call`; defaults to the bounded
    /// [`FuelLimit::default`] ([`DEFAULT_FUEL`], metered). Consumers that load blocks
    /// directly (e.g. gizza's `WasmiBlock::load_from_bytes_with_limits`) read it
    /// back via `Wafer::resource_limits()`.
    pub(crate) fuel: FuelLimit,
    /// Per-guest-call WASM linear-memory cap, in 64 KiB pages, applied to WASM
    /// blocks loaded through this runtime (`wafer.lock` blocks). Set via
    /// `WaferBuilder::max_wasm_memory_pages`; defaults to
    /// [`DEFAULT_MAX_WASM_MEMORY_PAGES`] (256 pages = 16 MiB). Consumers that
    /// load blocks directly read it back via `Wafer::resource_limits()`.
    pub(crate) max_wasm_memory_pages: u32,
    /// How engines this runtime creates compile modules — the shared engine
    /// and `wafer.lock` blocks — and the mode direct loaders read back via
    /// `Wafer::resource_limits()`. Set via `WaferBuilder::wasm_compilation`,
    /// which refuses a non-eager mode with metered fuel; defaults to
    /// [`WasmCompilation::Eager`].
    pub(crate) compilation: WasmCompilation,
    /// Shared fuel-metered WASM engine, created lazily by
    /// `Wafer::wasm_engine()` for embedders that compile blocks against one
    /// engine. The runtime's own `wafer.lock` loading does not use it.
    #[cfg(feature = "wasmi")]
    pub(crate) engine: Option<Arc<wasmi::Engine>>,
}

impl WasmState {
    /// Default state: noop asset loader, default fuel budget + memory cap, no
    /// engine yet.
    pub(crate) fn new() -> Self {
        Self {
            asset_loader: Arc::new(NoopAssetLoader),
            fuel: FuelLimit::default(),
            max_wasm_memory_pages: DEFAULT_MAX_WASM_MEMORY_PAGES,
            compilation: WasmCompilation::default(),
            #[cfg(feature = "wasmi")]
            engine: None,
        }
    }

    /// The runtime's configured per-call resource limits, bundled for direct
    /// loaders. Mirrors the individual `fuel` / `max_wasm_memory_pages` /
    /// `compilation` fields.
    pub(crate) fn resource_limits(&self) -> ResourceLimits {
        ResourceLimits {
            fuel: self.fuel,
            memory_pages: self.max_wasm_memory_pages,
            compilation: self.compilation,
            // SEC-03 host-side caps are not builder-configurable yet; use the
            // bounded defaults for runtime-loaded blocks.
            ..ResourceLimits::default()
        }
    }
}

#[cfg(feature = "wasmi")]
impl super::Wafer {
    /// Get or create the shared WASM engine.
    ///
    /// The engine's `consume_fuel` flag and compilation mode are derived
    /// from the runtime's configured [`FuelLimit`] and [`WasmCompilation`],
    /// so blocks an embedder loads against this engine meter (or don't) and
    /// compile consistently with the builder selection.
    pub fn wasm_engine(&mut self) -> Result<&wasmi::Engine, wafer_block::error::RuntimeError> {
        if self.wasm.engine.is_none() {
            let engine = wasmi::Engine::new(&wasmi_config(self.wasm.fuel, self.wasm.compilation)?);
            self.wasm.engine = Some(Arc::new(engine));
        }
        Ok(self
            .wasm
            .engine
            .as_ref()
            .expect("wasm_engine initialized above"))
    }
}
