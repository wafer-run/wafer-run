use std::sync::{Arc, Mutex};
use wasmtime::*;
use wasmtime::component::{Component, Linker};

use crate::block::{Block, BlockInfo};
use crate::common::ErrorCode;
use crate::context::Context;
use crate::types::*;
use super::capabilities::BlockCapabilities;
use super::bindings::WaferBlock;
use super::host::HostState;

/// Create a hardened Wasmtime engine with epoch interruption and component model enabled.
fn hardened_engine() -> Result<Engine, String> {
    let mut config = Config::new();
    config.epoch_interruption(true);
    config.wasm_component_model(true);
    Engine::new(&config).map_err(|e| format!("creating hardened engine: {e}"))
}

/// WASMBlock wraps a compiled WASM component and implements Block.
pub struct WASMBlock {
    engine: Engine,
    component: Component,
    linker: Linker<HostState>,
    info_cache: Mutex<Option<BlockInfo>>,
    capabilities: BlockCapabilities,
}

impl WASMBlock {
    /// Load a WASM block from a file path.
    pub fn load(path: &str) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("reading WASM file: {}", e))?;
        Self::load_from_bytes(&bytes)
    }

    /// Load a WASM block from raw bytes (backward-compatible: unrestricted capabilities).
    pub fn load_from_bytes(wasm_bytes: &[u8]) -> Result<Self, String> {
        Self::load_with_capabilities(wasm_bytes, BlockCapabilities::unrestricted())
    }

    /// Load with explicit capabilities.
    pub fn load_with_capabilities(wasm_bytes: &[u8], caps: BlockCapabilities) -> Result<Self, String> {
        let engine = hardened_engine()?;
        Self::build_from_engine(engine, wasm_bytes, caps)
    }

    /// Load with a shared engine and capabilities.
    pub fn load_with_engine(engine: &Engine, wasm_bytes: &[u8], caps: BlockCapabilities) -> Result<Self, String> {
        Self::build_from_engine(engine.clone(), wasm_bytes, caps)
    }

    fn build_from_engine(engine: Engine, wasm_bytes: &[u8], caps: BlockCapabilities) -> Result<Self, String> {
        let component = Component::new(&engine, wasm_bytes)
            .map_err(|e| format!("compiling WASM component: {}", e))?;

        let mut linker = Linker::new(&engine);

        // Add all host interface implementations to the linker.
        WaferBlock::add_to_linker(&mut linker, |state: &mut HostState| state)
            .map_err(|e| format!("linking host interfaces: {}", e))?;

        Ok(Self {
            engine,
            component,
            linker,
            info_cache: Mutex::new(None),
            capabilities: caps,
        })
    }

    fn create_instance(&self, ctx: Option<Arc<dyn Context>>) -> Result<(Store<HostState>, WaferBlock), String> {
        let mut store = Store::new(&self.engine, HostState {
            context: ctx,
            capabilities: self.capabilities.clone(),
        });
        store.set_epoch_deadline(10); // 10 epoch ticks = ~10 seconds with 1 tick/second
        let block = WaferBlock::instantiate(&mut store, &self.component, &self.linker)
            .map_err(|e| format!("instantiating WASM component: {}", e))?;
        Ok((store, block))
    }
}

impl Block for WASMBlock {
    fn block_capabilities(&self) -> Option<&BlockCapabilities> {
        Some(&self.capabilities)
    }

    fn info(&self) -> BlockInfo {
        // Check cache
        if let Ok(guard) = self.info_cache.lock() {
            if let Some(ref info) = *guard {
                return info.clone();
            }
        }

        let (mut store, block) = match self.create_instance(None) {
            Ok(r) => r,
            Err(e) => {
                return BlockInfo {
                    name: "unknown".to_string(),
                    version: "0.0.0".to_string(),
                    interface: "error".to_string(),
                    summary: format!("failed to create instance: {}", e),
                    instance_mode: InstanceMode::PerNode,
                    allowed_modes: Vec::new(),
                    admin_ui: None,
                };
            }
        };

        let wit_info = match block.wafer_block_world_block().call_info(&mut store) {
            Ok(i) => i,
            Err(e) => {
                return BlockInfo {
                    name: "unknown".to_string(),
                    version: "0.0.0".to_string(),
                    interface: "error".to_string(),
                    summary: format!("calling info failed: {}", e),
                    instance_mode: InstanceMode::PerNode,
                    allowed_modes: Vec::new(),
                    admin_ui: None,
                };
            }
        };

        let info = block_info_from_wit(wit_info);

        // Cache it
        if let Ok(mut guard) = self.info_cache.lock() {
            *guard = Some(info.clone());
        }

        info
    }

    fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        // SAFETY: The WASM call is synchronous — ctx outlives the call and the
        // Arc is dropped before this function returns.
        let ctx_arc: Arc<dyn Context> = unsafe {
            let ctx_static: *const (dyn Context + 'static) =
                std::mem::transmute(ctx as *const dyn Context);
            Arc::new(ContextWrapper(ctx_static))
        };

        let (mut store, block) = match self.create_instance(Some(ctx_arc)) {
            Ok(r) => r,
            Err(e) => {
                return msg.clone().err(WaferError::new(ErrorCode::INTERNAL, e));
            }
        };

        let wit_msg = message_to_wit(msg);

        let wit_result = match block.wafer_block_world_block().call_handle(&mut store, &wit_msg) {
            Ok(r) => r,
            Err(e) => {
                return msg.clone().err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("calling handle: {}", e),
                ));
            }
        };

        let mut result = result_from_wit(wit_result);
        // Preserve the guest's returned message if present; fall back to the
        // original only when the guest returned None.
        if result.message.is_none() {
            result.message = Some(msg.clone());
        }
        result
    }

    fn lifecycle(
        &self,
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        // SAFETY: Same as handle — synchronous call, ctx outlives it.
        let ctx_arc: Arc<dyn Context> = unsafe {
            let ctx_static: *const (dyn Context + 'static) =
                std::mem::transmute(ctx as *const dyn Context);
            Arc::new(ContextWrapper(ctx_static))
        };

        let (mut store, block_instance) = self
            .create_instance(Some(ctx_arc))
            .map_err(|e| WaferError::new(ErrorCode::INTERNAL, e))?;

        let wit_event = lifecycle_event_to_wit(&event);

        match block_instance.wafer_block_world_block().call_lifecycle(&mut store, &wit_event) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(wit_err)) => Err(WaferError::new(error_code_from_wit(wit_err.code), wit_err.message)),
            Err(e) => Err(WaferError::new(ErrorCode::INTERNAL, format!("calling lifecycle: {}", e))),
        }
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers between WIT types and internal types
// ---------------------------------------------------------------------------

use super::bindings;

fn message_to_wit(msg: &Message) -> bindings::types::Message {
    bindings::types::Message {
        kind: msg.kind.clone(),
        data: msg.data.clone(),
        meta: msg.meta.iter()
            .map(|(k, v)| bindings::types::MetaEntry { key: k.clone(), value: v.clone() })
            .collect(),
    }
}

fn message_from_wit(wm: bindings::types::Message) -> Message {
    let mut meta = std::collections::HashMap::new();
    for entry in wm.meta {
        meta.insert(entry.key, entry.value);
    }
    Message {
        kind: wm.kind,
        data: wm.data,
        meta,
    }
}

fn result_from_wit(wr: bindings::types::BlockResult) -> Result_ {
    let action = match wr.action {
        bindings::types::Action::Continue => Action::Continue,
        bindings::types::Action::Respond => Action::Respond,
        bindings::types::Action::Drop => Action::Drop,
        bindings::types::Action::Error => Action::Error,
    };

    let response = wr.response.map(|r| {
        let mut meta = std::collections::HashMap::new();
        for entry in r.meta {
            meta.insert(entry.key, entry.value);
        }
        Response { data: r.data, meta }
    });

    let error = wr.error.map(|e| {
        let mut meta = std::collections::HashMap::new();
        for entry in e.meta {
            meta.insert(entry.key, entry.value);
        }
        WaferError {
            code: error_code_from_wit(e.code).to_string(),
            message: e.message,
            meta,
        }
    });

    Result_ {
        action,
        response,
        error,
        message: wr.message.map(message_from_wit),
    }
}

fn block_info_from_wit(wbi: bindings::types::BlockInfo) -> BlockInfo {
    let instance_mode = match wbi.instance_mode {
        bindings::types::InstanceMode::PerNode => InstanceMode::PerNode,
        bindings::types::InstanceMode::Singleton => InstanceMode::Singleton,
        bindings::types::InstanceMode::PerChain => InstanceMode::PerChain,
        bindings::types::InstanceMode::PerExecution => InstanceMode::PerExecution,
    };

    let allowed_modes: Vec<InstanceMode> = wbi.allowed_modes.into_iter()
        .map(|m| match m {
            bindings::types::InstanceMode::PerNode => InstanceMode::PerNode,
            bindings::types::InstanceMode::Singleton => InstanceMode::Singleton,
            bindings::types::InstanceMode::PerChain => InstanceMode::PerChain,
            bindings::types::InstanceMode::PerExecution => InstanceMode::PerExecution,
        })
        .collect();

    BlockInfo {
        name: wbi.name,
        version: wbi.version,
        interface: wbi.interface,
        summary: wbi.summary,
        instance_mode,
        allowed_modes,
        admin_ui: None,
    }
}

/// Convert a WIT error-code enum to a string constant.
fn error_code_from_wit(code: bindings::types::ErrorCode) -> &'static str {
    match code {
        bindings::types::ErrorCode::Ok => ErrorCode::OK,
        bindings::types::ErrorCode::Cancelled => ErrorCode::CANCELLED,
        bindings::types::ErrorCode::Unknown => ErrorCode::UNKNOWN,
        bindings::types::ErrorCode::InvalidArgument => ErrorCode::INVALID_ARGUMENT,
        bindings::types::ErrorCode::DeadlineExceeded => ErrorCode::DEADLINE_EXCEEDED,
        bindings::types::ErrorCode::NotFound => ErrorCode::NOT_FOUND,
        bindings::types::ErrorCode::AlreadyExists => ErrorCode::ALREADY_EXISTS,
        bindings::types::ErrorCode::PermissionDenied => ErrorCode::PERMISSION_DENIED,
        bindings::types::ErrorCode::ResourceExhausted => ErrorCode::RESOURCE_EXHAUSTED,
        bindings::types::ErrorCode::FailedPrecondition => ErrorCode::FAILED_PRECONDITION,
        bindings::types::ErrorCode::Aborted => ErrorCode::ABORTED,
        bindings::types::ErrorCode::OutOfRange => ErrorCode::OUT_OF_RANGE,
        bindings::types::ErrorCode::Unimplemented => ErrorCode::UNIMPLEMENTED,
        bindings::types::ErrorCode::Internal => ErrorCode::INTERNAL,
        bindings::types::ErrorCode::Unavailable => ErrorCode::UNAVAILABLE,
        bindings::types::ErrorCode::DataLoss => ErrorCode::DATA_LOSS,
        bindings::types::ErrorCode::Unauthenticated => ErrorCode::UNAUTHENTICATED,
    }
}

fn lifecycle_event_to_wit(event: &LifecycleEvent) -> bindings::types::LifecycleEvent {
    let event_type = match event.event_type {
        LifecycleType::Init => bindings::types::LifecycleType::Init,
        LifecycleType::Start => bindings::types::LifecycleType::Start,
        LifecycleType::Stop => bindings::types::LifecycleType::Stop,
    };
    bindings::types::LifecycleEvent {
        event_type,
        data: event.data.clone(),
    }
}

// ---------------------------------------------------------------------------
// ContextWrapper: wrap a &dyn Context as an Arc<dyn Context>
// ---------------------------------------------------------------------------

struct ContextWrapper(*const dyn Context);
unsafe impl Send for ContextWrapper {}
unsafe impl Sync for ContextWrapper {}

impl Context for ContextWrapper {
    fn call_block(&self, block_name: &str, msg: &mut Message) -> Result_ {
        unsafe { &*self.0 }.call_block(block_name, msg)
    }

    fn is_cancelled(&self) -> bool {
        unsafe { &*self.0 }.is_cancelled()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        unsafe { &*self.0 }.config_get(key)
    }
}

impl From<ContextWrapper> for Arc<dyn Context> {
    fn from(w: ContextWrapper) -> Self {
        Arc::new(w)
    }
}
