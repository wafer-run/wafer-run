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
    /// directly (e.g. gizza's `WasmiBlock::load_from_bytes_with_fuel`) read it
    /// back via `Wafer::fuel_limit()`.
    pub(crate) fuel: FuelLimit,
    /// Shared fuel-metered WASM engine for all WASM blocks. Created lazily by
    /// `Wafer::wasm_engine()` (during `seal`).
    #[cfg(feature = "wasmi")]
    pub(crate) engine: Option<Arc<wasmi::Engine>>,
}

impl WasmState {
    /// Default state: noop asset loader, default fuel budget, no engine yet.
    pub(crate) fn new() -> Self {
        Self {
            asset_loader: Arc::new(NoopAssetLoader),
            fuel: FuelLimit::default(),
            #[cfg(feature = "wasmi")]
            engine: None,
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
