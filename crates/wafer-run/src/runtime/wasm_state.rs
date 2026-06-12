//! WASM runtime state: the shared fuel-metered engine (created lazily at
//! seal) and the host-injected asset loader propagated into WASM blocks.

use std::sync::Arc;

use crate::asset_loader::{LoadAssetCallback, NoopAssetLoader};

/// WASM-related runtime state, grouped out of the `Wafer` god-struct.
pub(crate) struct WasmState {
    /// Host-injected async loader for external wasm/js assets referenced by
    /// `BlockInfo::external_assets`. Defaults to `NoopAssetLoader`; hosts that
    /// need lazy asset loading call `Wafer::set_asset_loader` during startup.
    /// Propagated into each `WasmiBlock` at registration.
    pub(crate) asset_loader: Arc<dyn LoadAssetCallback>,
    /// Shared fuel-metered WASM engine for all WASM blocks. Created lazily by
    /// `Wafer::wasm_engine()` (during `seal`).
    #[cfg(feature = "wasmi")]
    pub(crate) engine: Option<Arc<wasmi::Engine>>,
}

impl WasmState {
    /// Default state: noop asset loader, no engine yet.
    pub(crate) fn new() -> Self {
        Self {
            asset_loader: Arc::new(NoopAssetLoader),
            #[cfg(feature = "wasmi")]
            engine: None,
        }
    }
}

#[cfg(feature = "wasmi")]
impl super::Wafer {
    /// Get or create the shared WASM engine.
    pub fn wasm_engine(&mut self) -> Result<&wasmi::Engine, crate::error::RuntimeError> {
        if self.wasm.engine.is_none() {
            let mut config = wasmi::Config::default();
            config.consume_fuel(true);
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
