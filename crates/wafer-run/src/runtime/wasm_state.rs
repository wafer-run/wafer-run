//! WASM runtime state: the shared fuel-metered engine (created lazily at
//! seal) and the host-injected asset loader propagated into WASM blocks.

use std::sync::Arc;

use crate::asset_loader::{LoadAssetCallback, NoopAssetLoader};

/// Default per-guest-call fuel budget in wasmi fuel units. A single guest
/// invocation that consumes more than this traps with a fuel-exhaustion error.
///
/// This is the value used by [`FuelLimit::default`] and matches the historical
/// hard-coded cap, so the default runtime behaviour is unchanged.
pub const DEFAULT_FUEL: u64 = 100_000_000;

/// Default per-guest-call WASM linear-memory cap, in 64 KiB wasmi pages
/// (256 pages = 16 MiB).
///
/// This is the value used by [`ResourceLimits::default`] and matches the
/// historical hard-coded cap (`MAX_WASM_MEMORY_PAGES`), so a guest's memory
/// growth is bounded identically to before unless a consumer raises the cap.
pub const DEFAULT_MAX_WASM_MEMORY_PAGES: u32 = 256;

/// Default per-guest-call aggregate cap on host-owned bytes copied *out* of
/// guest linear memory — stream request buffers plus attachment payloads
/// (SEC-03). The wasmi linear-memory cap does not cover these host `Vec`s, so
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
/// [`Metered(DEFAULT_FUEL)`](FuelLimit::Metered) — i.e. the historical 100M
/// cap — so omitting `fuel_per_call` leaves behaviour byte-for-byte unchanged.
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
/// (100M metered fuel, 256-page / 16 MiB memory). Trusted single-user
/// embedders raise either bound: [`FuelLimit::Unmetered`] for heavy compute,
/// or a larger `memory_pages` for memory-heavy tools (e.g. the `syntect`+font
/// code-screenshot tool needs ~24 MiB ≈ 384 pages). The default is
/// byte-for-byte the historical behaviour, so omitting `ResourceLimits` (or
/// using [`default`](Self::default)) changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Per-guest-call wasmi fuel budget. See [`FuelLimit`].
    pub fuel: FuelLimit,
    /// Maximum WASM linear-memory size, in 64 KiB wasmi pages. A guest whose
    /// `memory.grow` would exceed this is denied (the grow returns -1).
    /// Defaults to [`DEFAULT_MAX_WASM_MEMORY_PAGES`] (256 pages = 16 MiB).
    pub memory_pages: u32,
    /// SEC-03: aggregate cap on host-owned bytes copied out of guest memory
    /// (stream request buffers + attachments) per guest call. Defaults to
    /// [`DEFAULT_MAX_HOST_BYTES`] (256 MiB).
    pub max_host_bytes: usize,
    /// SEC-03: cap on concurrent live streams per guest call. Defaults to
    /// [`DEFAULT_MAX_LIVE_STREAMS`] (1024).
    pub max_live_streams: usize,
    /// SEC-03: cap on WebAssembly table elements. Defaults to
    /// [`DEFAULT_MAX_TABLE_ELEMENTS`] (100_000).
    pub max_table_elements: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            fuel: FuelLimit::default(),
            memory_pages: DEFAULT_MAX_WASM_MEMORY_PAGES,
            max_host_bytes: DEFAULT_MAX_HOST_BYTES,
            max_live_streams: DEFAULT_MAX_LIVE_STREAMS,
            max_table_elements: DEFAULT_MAX_TABLE_ELEMENTS,
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
    /// [`FuelLimit::default`] (100M, metered). Consumers that load blocks
    /// directly (e.g. gizza's `WasmiBlock::load_from_bytes_with_limits`) read it
    /// back via `Wafer::resource_limits()`.
    pub(crate) fuel: FuelLimit,
    /// Per-guest-call WASM linear-memory cap, in 64 KiB pages, applied to WASM
    /// blocks loaded through this runtime (remote/lockfile blocks). Set via
    /// `WaferBuilder::max_wasm_memory_pages`; defaults to
    /// [`DEFAULT_MAX_WASM_MEMORY_PAGES`] (256 pages = 16 MiB). Consumers that
    /// load blocks directly read it back via `Wafer::resource_limits()`.
    pub(crate) max_wasm_memory_pages: u32,
    /// Shared fuel-metered WASM engine for all WASM blocks. Created lazily by
    /// `Wafer::wasm_engine()` (during `seal`).
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
            #[cfg(feature = "wasmi")]
            engine: None,
        }
    }

    /// The runtime's configured per-call resource limits, bundled for direct
    /// loaders. Mirrors the individual `fuel` / `max_wasm_memory_pages` fields.
    pub(crate) fn resource_limits(&self) -> ResourceLimits {
        ResourceLimits {
            fuel: self.fuel,
            memory_pages: self.max_wasm_memory_pages,
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
    /// The engine's `consume_fuel` flag is derived from the runtime's
    /// configured [`FuelLimit`] so remote/lockfile blocks loaded against this
    /// engine meter (or don't) consistently with the builder selection.
    pub fn wasm_engine(&mut self) -> Result<&wasmi::Engine, wafer_block::error::RuntimeError> {
        if self.wasm.engine.is_none() {
            let mut config = wasmi::Config::default();
            config.consume_fuel(self.wasm.fuel.consume_fuel());
            let engine = wasmi::Engine::new(&config);
            self.wasm.engine = Some(Arc::new(engine));
        }
        Ok(self
            .wasm
            .engine
            .as_ref()
            .expect("wasm_engine initialized above"))
    }
}
