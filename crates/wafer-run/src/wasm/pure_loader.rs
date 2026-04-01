use std::sync::Mutex;
use wasmtime::{component::*, Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

use crate::block::BlockInfo;
use crate::types::InstanceMode;

bindgen!({
    world: "wafer-pure-block",
    path: "../../wit/wit",
    async: true,
});

/// Default fuel budget for pure block WASM execution.
const DEFAULT_FUEL: u64 = 100_000_000;

/// Maximum WASM memory: 256 pages = 16 MiB (each page is 64 KiB).
const MAX_WASM_MEMORY_PAGES: u64 = 256;

/// Create a wasmtime Engine for pure blocks (no host imports needed).
fn pure_engine() -> Engine {
    let mut config = Config::default();
    config.consume_fuel(true);
    config.wasm_component_model(true);
    config.async_support(true);
    config.max_wasm_stack(1024 * 1024); // 1 MiB stack limit
    Engine::new(&config).expect("failed to create wasmtime engine")
}

struct PureHostState {
    limiter: StoreLimits,
}

/// A simplified WASM loader for pure blocks.
///
/// Pure blocks have no host imports — they just receive input bytes
/// and return output bytes. This makes them lighter than full WASMBlocks.
pub struct PureWASMBlock {
    engine: Engine,
    component: Component,
    info_cache: Mutex<Option<BlockInfo>>,
}

impl PureWASMBlock {
    /// Load a pure block from WASM bytes.
    pub fn load_from_bytes(wasm_bytes: &[u8]) -> Result<Self, String> {
        let engine = pure_engine();
        Self::load_with_engine(&engine, wasm_bytes)
    }

    /// Load a pure block from WASM bytes with a shared engine.
    pub fn load_with_engine(engine: &Engine, wasm_bytes: &[u8]) -> Result<Self, String> {
        let component = Component::from_binary(engine, wasm_bytes)
            .map_err(|e| format!("failed to load pure block WASM: {e}"))?;
        Ok(Self {
            engine: engine.clone(),
            component,
            info_cache: Mutex::new(None),
        })
    }

    /// Instantiate the pure block component.
    async fn instantiate(&self) -> Result<(Store<PureHostState>, WaferPureBlock), String> {
        let limiter = StoreLimitsBuilder::new()
            .memory_size(MAX_WASM_MEMORY_PAGES as usize * 65536)
            .build();
        let mut store = Store::new(&self.engine, PureHostState { limiter });
        store.limiter(|state| &mut state.limiter);
        store
            .set_fuel(DEFAULT_FUEL)
            .map_err(|e| format!("failed to set fuel: {e}"))?;

        let linker = Linker::new(&self.engine);
        let block =
            WaferPureBlock::instantiate_async(&mut store, &self.component, &linker)
                .await
                .map_err(|e| format!("failed to instantiate pure block: {e}"))?;

        Ok((store, block))
    }

    /// Execute the pure block with JSON input bytes, returning JSON output bytes.
    pub async fn handle(&self, input: &[u8]) -> Result<Vec<u8>, String> {
        let (mut store, block) = self.instantiate().await?;
        let result = block
            .wafer_block_world_pure_block()
            .call_handle(&mut store, input)
            .await
            .map_err(|e| format!("pure block handle trap: {e}"))?;
        result
    }

    /// Get block info (cached after first call).
    pub async fn info(&self) -> Result<BlockInfo, String> {
        {
            let cache = self.info_cache.lock().unwrap();
            if let Some(info) = cache.as_ref() {
                return Ok(info.clone());
            }
        }

        let (mut store, block) = self.instantiate().await?;
        let info_json = block
            .wafer_block_world_pure_block()
            .call_info(&mut store)
            .await
            .map_err(|e| format!("pure block info trap: {e}"))?;

        // Parse the JSON-encoded BlockDef into a BlockInfo
        let def: wafer_flow::BlockDef =
            serde_json::from_str(&info_json).map_err(|e| format!("invalid block info JSON: {e}"))?;

        let info = BlockInfo::new(def.name, def.version, "", def.description.unwrap_or_default())
            .instance_mode(InstanceMode::PerExecution);

        {
            let mut cache = self.info_cache.lock().unwrap();
            *cache = Some(info.clone());
        }

        Ok(info)
    }
}
