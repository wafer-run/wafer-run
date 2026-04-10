use std::sync::Mutex;

use async_trait::async_trait;

use crate::block::{Block, BlockInfo};
use crate::context::Context;
use crate::types::*;
use wafer_block::helpers::MessageExt;

use super::capabilities::BlockCapabilities;

pub struct WasmiBlock {
    engine: wasmi::Engine,
    module: wasmi::Module,
    info_cache: Mutex<Option<BlockInfo>>,
    capabilities: BlockCapabilities,
}

impl WasmiBlock {
    pub fn load(path: &str) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("reading WASM file: {e}"))?;
        Self::load_from_bytes(&bytes)
    }

    pub fn load_from_bytes(wasm_bytes: &[u8]) -> Result<Self, String> {
        Self::load_with_capabilities(wasm_bytes, BlockCapabilities::unrestricted())
    }

    pub fn load_with_capabilities(
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
    ) -> Result<Self, String> {
        let engine = wasmi::Engine::default();
        let module = wasmi::Module::new(&engine, wasm_bytes)
            .map_err(|e| format!("compiling WASM module: {e}"))?;
        Ok(Self {
            engine,
            module,
            info_cache: Mutex::new(None),
            capabilities: caps,
        })
    }

    pub fn load_with_engine(
        engine: &wasmi::Engine,
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
    ) -> Result<Self, String> {
        let module = wasmi::Module::new(engine, wasm_bytes)
            .map_err(|e| format!("compiling WASM module: {e}"))?;
        Ok(Self {
            engine: engine.clone(),
            module,
            info_cache: Mutex::new(None),
            capabilities: caps,
        })
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl Block for WasmiBlock {
    fn info(&self) -> BlockInfo {
        if let Ok(guard) = self.info_cache.lock() {
            if let Some(ref info) = *guard {
                return info.clone();
            }
        }
        BlockInfo::new("stub", "0.0.0", "stub", "wasmi loader stub")
    }

    async fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        msg.clone().err(WaferError {
            code: ErrorCode::Unimplemented,
            message: "wasmi loader not yet implemented".into(),
            meta: vec![],
        })
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }

    fn block_capabilities(&self) -> Option<&BlockCapabilities> {
        Some(&self.capabilities)
    }
}
