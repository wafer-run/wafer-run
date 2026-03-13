use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use wasmtime::{Engine, Config, Store, component::*};

use crate::block::{Block, BlockInfo};
use crate::context::Context;
use crate::types::*;
use super::capabilities::BlockCapabilities;
use super::host::HostState;
use wafer_block::helpers::MessageExt;

bindgen!({
    world: "wafer-block",
    path: "../../wit/wit",
    async: true,
    trappable_imports: true,
});

use wafer::block_world::runtime::Host;

/// Default fuel budget for WASM execution (~100M instructions).
const DEFAULT_FUEL: u64 = 100_000_000;

/// Create a wasmtime Engine with fuel metering enabled.
fn fuel_engine() -> Engine {
    let mut config = Config::default();
    config.consume_fuel(true);
    config.wasm_component_model(true);
    config.async_support(true);
    Engine::new(&config).expect("failed to create wasmtime engine")
}

// ---------------------------------------------------------------------------
// Host trait implementation (runtime → WASM imports)
// ---------------------------------------------------------------------------

#[async_trait]
impl Host for HostState {
    async fn is_cancelled(&mut self) -> wasmtime::Result<bool> {
        Ok(self.context
            .as_ref()
            .map(|ctx| ctx.is_cancelled())
            .unwrap_or(false))
    }

    async fn log(&mut self, level: String, msg: String) -> wasmtime::Result<()> {
        match level.as_str() {
            "debug" => tracing::debug!("{}", msg),
            "info" => tracing::info!("{}", msg),
            "warn" => tracing::warn!("{}", msg),
            "error" => tracing::error!("{}", msg),
            _ => tracing::info!("{}", msg),
        }
        Ok(())
    }

    async fn call_block(
        &mut self,
        block_name: String,
        msg: wafer::block_world::types::Message,
    ) -> wasmtime::Result<wafer::block_world::types::BlockResult> {
        let ctx = match self.context.as_ref() {
            Some(c) => c.clone(),
            None => {
                return Ok(wafer::block_world::types::BlockResult {
                    action: wafer::block_world::types::Action::Error,
                    response: None,
                    error: Some(wafer::block_world::types::WaferError {
                        code: wafer::block_world::types::ErrorCode::Internal,
                        message: "no context for call_block".to_string(),
                        meta: vec![],
                    }),
                    message: Some(msg.clone()),
                });
            }
        };

        let mut rust_msg = to_runtime_message(&msg);
        let result = ctx.call_block(&block_name, &mut rust_msg).await;
        Ok(to_guest_result(&result))
    }
}

// ---------------------------------------------------------------------------
// WASMBlock — loads and runs a WASM Component block via wasmtime
// ---------------------------------------------------------------------------

pub struct WASMBlock {
    engine: Engine,
    component: Component,
    info_cache: Mutex<Option<BlockInfo>>,
    capabilities: BlockCapabilities,
}

impl WASMBlock {
    pub fn load(path: &str) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("reading WASM file: {}", e))?;
        Self::load_from_bytes(&bytes)
    }

    pub fn load_from_bytes(wasm_bytes: &[u8]) -> Result<Self, String> {
        Self::load_with_capabilities(wasm_bytes, BlockCapabilities::unrestricted())
    }

    pub fn load_with_capabilities(wasm_bytes: &[u8], caps: BlockCapabilities) -> Result<Self, String> {
        let engine = fuel_engine();
        Self::build_from_engine(engine, wasm_bytes, caps)
    }

    pub fn load_with_engine(engine: &Engine, wasm_bytes: &[u8], caps: BlockCapabilities) -> Result<Self, String> {
        Self::build_from_engine(engine.clone(), wasm_bytes, caps)
    }

    fn build_from_engine(engine: Engine, wasm_bytes: &[u8], caps: BlockCapabilities) -> Result<Self, String> {
        let component = Component::new(&engine, wasm_bytes)
            .map_err(|e| format!("compiling WASM component: {}", e))?;

        Ok(Self {
            engine,
            component,
            info_cache: Mutex::new(None),
            capabilities: caps,
        })
    }

    async fn instantiate(&self, ctx: Option<Arc<dyn Context>>) -> Result<(Store<HostState>, WaferBlock), String> {
        let mut store = Store::new(
            &self.engine,
            HostState {
                context: ctx,
                capabilities: self.capabilities.clone(),
            },
        );
        store.set_fuel(DEFAULT_FUEL).map_err(|e| format!("setting fuel: {e}"))?;

        let mut linker = Linker::new(&self.engine);
        wafer::block_world::runtime::add_to_linker(&mut linker, |s| s)
            .map_err(|e| format!("failed to add host runtime to linker: {}", e))?;

        let bindings = WaferBlock::instantiate_async(&mut store, &self.component, &linker)
            .await
            .map_err(|e| format!("instantiating WASM component: {}", e))?;

        Ok((store, bindings))
    }
}

// ---------------------------------------------------------------------------
// Type conversion: wasmtime bindgen types ↔ runtime (wafer-block) types
//
// Both are generated from the same .wit files but by different code generators
// (wasmtime::component::bindgen! vs wit_bindgen::generate!), so they are
// structurally identical but distinct Rust types.
// ---------------------------------------------------------------------------

fn to_runtime_error_code(c: wafer::block_world::types::ErrorCode) -> ErrorCode {
    match c {
        wafer::block_world::types::ErrorCode::Ok => ErrorCode::Ok,
        wafer::block_world::types::ErrorCode::Cancelled => ErrorCode::Cancelled,
        wafer::block_world::types::ErrorCode::Unknown => ErrorCode::Unknown,
        wafer::block_world::types::ErrorCode::InvalidArgument => ErrorCode::InvalidArgument,
        wafer::block_world::types::ErrorCode::DeadlineExceeded => ErrorCode::DeadlineExceeded,
        wafer::block_world::types::ErrorCode::NotFound => ErrorCode::NotFound,
        wafer::block_world::types::ErrorCode::AlreadyExists => ErrorCode::AlreadyExists,
        wafer::block_world::types::ErrorCode::PermissionDenied => ErrorCode::PermissionDenied,
        wafer::block_world::types::ErrorCode::ResourceExhausted => ErrorCode::ResourceExhausted,
        wafer::block_world::types::ErrorCode::FailedPrecondition => ErrorCode::FailedPrecondition,
        wafer::block_world::types::ErrorCode::Aborted => ErrorCode::Aborted,
        wafer::block_world::types::ErrorCode::OutOfRange => ErrorCode::OutOfRange,
        wafer::block_world::types::ErrorCode::Unimplemented => ErrorCode::Unimplemented,
        wafer::block_world::types::ErrorCode::Internal => ErrorCode::Internal,
        wafer::block_world::types::ErrorCode::Unavailable => ErrorCode::Unavailable,
        wafer::block_world::types::ErrorCode::DataLoss => ErrorCode::DataLoss,
        wafer::block_world::types::ErrorCode::Unauthenticated => ErrorCode::Unauthenticated,
    }
}

fn to_guest_error_code(c: &ErrorCode) -> wafer::block_world::types::ErrorCode {
    match c {
        ErrorCode::Ok => wafer::block_world::types::ErrorCode::Ok,
        ErrorCode::Cancelled => wafer::block_world::types::ErrorCode::Cancelled,
        ErrorCode::Unknown => wafer::block_world::types::ErrorCode::Unknown,
        ErrorCode::InvalidArgument => wafer::block_world::types::ErrorCode::InvalidArgument,
        ErrorCode::DeadlineExceeded => wafer::block_world::types::ErrorCode::DeadlineExceeded,
        ErrorCode::NotFound => wafer::block_world::types::ErrorCode::NotFound,
        ErrorCode::AlreadyExists => wafer::block_world::types::ErrorCode::AlreadyExists,
        ErrorCode::PermissionDenied => wafer::block_world::types::ErrorCode::PermissionDenied,
        ErrorCode::ResourceExhausted => wafer::block_world::types::ErrorCode::ResourceExhausted,
        ErrorCode::FailedPrecondition => wafer::block_world::types::ErrorCode::FailedPrecondition,
        ErrorCode::Aborted => wafer::block_world::types::ErrorCode::Aborted,
        ErrorCode::OutOfRange => wafer::block_world::types::ErrorCode::OutOfRange,
        ErrorCode::Unimplemented => wafer::block_world::types::ErrorCode::Unimplemented,
        ErrorCode::Internal => wafer::block_world::types::ErrorCode::Internal,
        ErrorCode::Unavailable => wafer::block_world::types::ErrorCode::Unavailable,
        ErrorCode::DataLoss => wafer::block_world::types::ErrorCode::DataLoss,
        ErrorCode::Unauthenticated => wafer::block_world::types::ErrorCode::Unauthenticated,
    }
}

fn to_runtime_meta(meta: Vec<wafer::block_world::types::MetaEntry>) -> Vec<MetaEntry> {
    meta.into_iter()
        .map(|e| MetaEntry { key: e.key, value: e.value })
        .collect()
}

fn to_guest_meta(meta: &[MetaEntry]) -> Vec<wafer::block_world::types::MetaEntry> {
    meta.iter()
        .map(|e| wafer::block_world::types::MetaEntry {
            key: e.key.clone(),
            value: e.value.clone(),
        })
        .collect()
}

fn to_runtime_action(a: wafer::block_world::types::Action) -> Action {
    match a {
        wafer::block_world::types::Action::Continue => Action::Continue,
        wafer::block_world::types::Action::Respond => Action::Respond,
        wafer::block_world::types::Action::Drop => Action::Drop,
        wafer::block_world::types::Action::Error => Action::Error,
    }
}

fn to_guest_action(a: &Action) -> wafer::block_world::types::Action {
    match a {
        Action::Continue => wafer::block_world::types::Action::Continue,
        Action::Respond => wafer::block_world::types::Action::Respond,
        Action::Drop => wafer::block_world::types::Action::Drop,
        Action::Error => wafer::block_world::types::Action::Error,
    }
}

fn to_runtime_message(msg: &wafer::block_world::types::Message) -> Message {
    Message {
        kind: msg.kind.clone(),
        data: msg.data.clone(),
        meta: to_runtime_meta(msg.meta.clone()),
    }
}

fn to_guest_message(msg: &Message) -> wafer::block_world::types::Message {
    wafer::block_world::types::Message {
        kind: msg.kind.clone(),
        data: msg.data.clone(),
        meta: to_guest_meta(&msg.meta),
    }
}

fn to_runtime_result(r: wafer::block_world::types::BlockResult) -> Result_ {
    Result_ {
        action: to_runtime_action(r.action),
        response: r.response.map(|resp| Response {
            data: resp.data,
            meta: to_runtime_meta(resp.meta),
        }),
        error: r.error.map(|err| WaferError {
            code: to_runtime_error_code(err.code),
            message: err.message,
            meta: to_runtime_meta(err.meta),
        }),
        message: r.message.map(|msg| Message {
            kind: msg.kind,
            data: msg.data,
            meta: to_runtime_meta(msg.meta),
        }),
    }
}

fn to_guest_result(r: &Result_) -> wafer::block_world::types::BlockResult {
    wafer::block_world::types::BlockResult {
        action: to_guest_action(&r.action),
        response: r.response.as_ref().map(|resp| wafer::block_world::types::Response {
            data: resp.data.clone(),
            meta: to_guest_meta(&resp.meta),
        }),
        error: r.error.as_ref().map(|err| wafer::block_world::types::WaferError {
            code: to_guest_error_code(&err.code),
            message: err.message.clone(),
            meta: to_guest_meta(&err.meta),
        }),
        message: r.message.as_ref().map(|msg| wafer::block_world::types::Message {
            kind: msg.kind.clone(),
            data: msg.data.clone(),
            meta: to_guest_meta(&msg.meta),
        }),
    }
}

fn to_runtime_instance_mode(m: wafer::block_world::types::InstanceMode) -> InstanceMode {
    match m {
        wafer::block_world::types::InstanceMode::PerNode => InstanceMode::PerNode,
        wafer::block_world::types::InstanceMode::Singleton => InstanceMode::Singleton,
        wafer::block_world::types::InstanceMode::PerFlow => InstanceMode::PerFlow,
        wafer::block_world::types::InstanceMode::PerExecution => InstanceMode::PerExecution,
    }
}

// ---------------------------------------------------------------------------
// ContextGuard — safe wrapper for passing Context across async WASM calls
// ---------------------------------------------------------------------------

struct ContextGuard {
    arc: Arc<dyn Context>,
}

impl ContextGuard {
    fn new(ctx: &dyn Context) -> Self {
        let ptr: *const dyn Context = ctx;
        let ctx_static: *const (dyn Context + 'static) = unsafe { std::mem::transmute(ptr) };
        Self {
            arc: Arc::new(ContextWrapper(ctx_static)),
        }
    }

    fn as_arc(&self) -> Arc<dyn Context> {
        self.arc.clone()
    }
}

struct ContextWrapper(*const dyn Context);
unsafe impl Send for ContextWrapper {}
unsafe impl Sync for ContextWrapper {}

#[async_trait]
impl Context for ContextWrapper {
    async fn call_block(&self, block_name: &str, msg: &mut Message) -> Result_ {
        unsafe { &*self.0 }.call_block(block_name, msg).await
    }

    fn is_cancelled(&self) -> bool {
        unsafe { &*self.0 }.is_cancelled()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        unsafe { &*self.0 }.config_get(key)
    }

    fn registered_blocks(&self) -> Vec<BlockInfo> {
        unsafe { &*self.0 }.registered_blocks()
    }

    fn flow_infos(&self) -> Vec<crate::config::FlowInfo> {
        unsafe { &*self.0 }.flow_infos()
    }

    fn flow_defs(&self) -> Vec<crate::config::FlowDef> {
        unsafe { &*self.0 }.flow_defs()
    }
}

// ---------------------------------------------------------------------------
// Block trait impl for WASMBlock
// ---------------------------------------------------------------------------

#[async_trait]
impl Block for WASMBlock {
    fn block_capabilities(&self) -> Option<&BlockCapabilities> {
        Some(&self.capabilities)
    }

    fn info(&self) -> BlockInfo {
        if let Ok(guard) = self.info_cache.lock() {
            if let Some(ref info) = *guard {
                return info.clone();
            }
        }

        // Clone what we need for the spawned thread (avoids borrowing self across thread boundary).
        let engine = self.engine.clone();
        let component = self.component.clone();
        let capabilities = self.capabilities.clone();

        let info = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                let mut store = Store::new(
                    &engine,
                    HostState {
                        context: None,
                        capabilities,
                    },
                );
                store.set_fuel(DEFAULT_FUEL).map_err(|e| format!("setting fuel: {e}"))?;
                let mut linker = Linker::new(&engine);
                wafer::block_world::runtime::add_to_linker(&mut linker, |s| s)
                    .map_err(|e| format!("failed to add host runtime to linker: {}", e))?;
                let bindings = WaferBlock::instantiate_async(&mut store, &component, &linker)
                    .await
                    .map_err(|e| format!("instantiating WASM component: {}", e))?;

                let info = bindings.wafer_block_world_block().call_info(&mut store).await
                    .map_err(|e| format!("calling block.info(): {e}"))?;

                Ok::<BlockInfo, String>(BlockInfo {
                    name: info.name,
                    version: info.version,
                    interface: info.interface,
                    summary: info.summary,
                    instance_mode: to_runtime_instance_mode(info.instance_mode),
                    allowed_modes: info.allowed_modes.into_iter().map(to_runtime_instance_mode).collect(),
                    admin_ui: None,
                    runtime: BlockRuntime::Wasm,
                    requires: vec![],
                })
            })
        }).join().unwrap().unwrap_or_else(|e| BlockInfo {
            name: "unknown".to_string(),
            version: "0.0.0".to_string(),
            interface: "error".to_string(),
            summary: format!("failed to get info: {}", e),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: BlockRuntime::Wasm,
            requires: Vec::new(),
        });

        if let Ok(mut guard) = self.info_cache.lock() {
            *guard = Some(info.clone());
        }

        info
    }

    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let guard = ContextGuard::new(ctx);

        let (mut store, bindings) = match self.instantiate(Some(guard.as_arc())).await {
            Ok(r) => r,
            Err(e) => return msg.clone().err(WaferError { code: ErrorCode::Internal, message: e, meta: vec![] }),
        };

        let guest_msg = to_guest_message(msg);
        let result = match bindings.wafer_block_world_block().call_handle(&mut store, &guest_msg).await {
            Ok(r) => r,
            Err(e) => return msg.clone().err(WaferError { code: ErrorCode::Internal, message: format!("calling block.handle(): {e}"), meta: vec![] }),
        };

        to_runtime_result(result)
    }

    async fn lifecycle(
        &self,
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        let guard = ContextGuard::new(ctx);

        let (mut store, bindings) = self
            .instantiate(Some(guard.as_arc()))
            .await
            .map_err(|e| WaferError { code: ErrorCode::Internal, message: e, meta: vec![] })?;

        let guest_event = wafer::block_world::types::LifecycleEvent {
            event_type: match event.event_type {
                LifecycleType::Init => wafer::block_world::types::LifecycleType::Init,
                LifecycleType::Start => wafer::block_world::types::LifecycleType::Start,
                LifecycleType::Stop => wafer::block_world::types::LifecycleType::Stop,
            },
            data: event.data,
        };

        match bindings.wafer_block_world_block().call_lifecycle(&mut store, &guest_event).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(WaferError {
                code: to_runtime_error_code(e.code),
                message: e.message,
                meta: to_runtime_meta(e.meta),
            }),
            Err(e) => Err(WaferError { code: ErrorCode::Internal, message: format!("calling block.lifecycle(): {e}"), meta: vec![] }),
        }
    }
}
