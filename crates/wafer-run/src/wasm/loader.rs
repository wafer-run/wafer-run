use std::sync::{Arc, Mutex};

use wasmi::{Store, Engine, Module, Config, Linker, Caller, Instance};

use crate::block::{Block, BlockInfo};
use crate::common::ErrorCode;
use crate::context::Context;
use crate::types::*;
use super::capabilities::BlockCapabilities;
use super::host::{self, HostState};

/// Poll a future exactly once, expecting it to resolve immediately.
///
/// This exists ONLY for the wasmi host function boundary: wasmi host imports
/// are synchronous callbacks, so `call_block` (which is async) must be polled
/// here. All other code paths use proper `.await`.
fn poll_once_sync<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |p| RawWaker::new(p, &VTABLE),
        |_| {},
        |_| {},
        |_| {},
    );

    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);

    let mut fut = fut;
    let mut pinned = unsafe { std::pin::Pin::new_unchecked(&mut fut) };

    match pinned.as_mut().poll(&mut cx) {
        Poll::Ready(val) => val,
        Poll::Pending => panic!(
            "poll_once_sync: future returned Pending in wasmi host function — \
             the called block must not perform real async I/O from WASM context"
        ),
    }
}

/// Default fuel budget for WASM execution (~100M instructions).
const DEFAULT_FUEL: u64 = 100_000_000;

/// Create a wasmi Engine with fuel metering enabled.
fn fuel_engine() -> Engine {
    let mut config = Config::default();
    config.consume_fuel(true);
    Engine::new(&config)
}

/// WASMBlock wraps a compiled WASM module and implements Block via thin ABI.
///
/// # Thin ABI contract
///
/// Guest exports:
/// - `__wafer_alloc(len: i32) -> i32` — allocate `len` bytes, return ptr
/// - `__wafer_info() -> i64` — return packed ptr|len of JSON BlockInfo
/// - `__wafer_handle(ptr: i32, len: i32) -> i64` — handle message, return packed ptr|len of JSON BlockResult
/// - `__wafer_lifecycle(ptr: i32, len: i32) -> i64` — lifecycle event, return packed ptr|len of JSON result
///
/// Host imports (namespace `wafer`):
/// - `wafer.is_cancelled() -> i32` — 1 if cancelled, 0 otherwise
/// - `wafer.log(level_ptr: i32, level_len: i32, msg_ptr: i32, msg_len: i32)` — log a message
/// - `wafer.call_block(name_ptr: i32, name_len: i32, msg_ptr: i32, msg_len: i32) -> i64` — call another block
pub struct WASMBlock {
    engine: Engine,
    module: Module,
    info_cache: Mutex<Option<BlockInfo>>,
    capabilities: BlockCapabilities,
}

impl WASMBlock {
    /// Load a WASM block from a file path.
    pub fn load(path: &str) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("reading WASM file: {}", e))?;
        Self::load_from_bytes(&bytes)
    }

    /// Load a WASM block from raw bytes (unrestricted capabilities).
    pub fn load_from_bytes(wasm_bytes: &[u8]) -> Result<Self, String> {
        Self::load_with_capabilities(wasm_bytes, BlockCapabilities::unrestricted())
    }

    /// Load with explicit capabilities.
    pub fn load_with_capabilities(wasm_bytes: &[u8], caps: BlockCapabilities) -> Result<Self, String> {
        let engine = fuel_engine();
        Self::build_from_engine(engine, wasm_bytes, caps)
    }

    /// Load with a shared engine and capabilities.
    pub fn load_with_engine(engine: &Engine, wasm_bytes: &[u8], caps: BlockCapabilities) -> Result<Self, String> {
        Self::build_from_engine(engine.clone(), wasm_bytes, caps)
    }

    fn build_from_engine(engine: Engine, wasm_bytes: &[u8], caps: BlockCapabilities) -> Result<Self, String> {
        let module = Module::new(&engine, wasm_bytes)
            .map_err(|e| format!("compiling WASM module: {}", e))?;

        Ok(Self {
            engine,
            module,
            info_cache: Mutex::new(None),
            capabilities: caps,
        })
    }

    /// Create a new store + instance with host imports linked.
    fn create_instance(&self, ctx: Option<Arc<dyn Context>>) -> Result<(Store<HostState>, Instance), String> {
        let fuel = ctx
            .as_ref()
            .and_then(|c| c.config_get("wasm_fuel"))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_FUEL);

        let mut store = Store::new(
            &self.engine,
            HostState {
                context: ctx,
                capabilities: self.capabilities.clone(),
            },
        );
        store.set_fuel(fuel).map_err(|e| format!("setting fuel: {e}"))?;

        let mut linker = Linker::new(&self.engine);
        self.link_host_functions(&mut linker)?;

        let instance = linker
            .instantiate(&mut store, &self.module)
            .and_then(|i| i.start(&mut store))
            .map_err(|e| format!("instantiating WASM module: {}", e))?;

        Ok((store, instance))
    }

    /// Register host import functions in the `wafer` namespace.
    fn link_host_functions(&self, linker: &mut Linker<HostState>) -> Result<(), String> {
        // wafer.is_cancelled() -> i32
        linker
            .func_wrap("wafer", "is_cancelled", |caller: Caller<'_, HostState>| -> i32 {
                let cancelled = caller
                    .data()
                    .context
                    .as_ref()
                    .map(|ctx| ctx.is_cancelled())
                    .unwrap_or(false);
                cancelled as i32
            })
            .map_err(|e| format!("linking is_cancelled: {e}"))?;

        // wafer.log(level_ptr, level_len, msg_ptr, msg_len)
        linker
            .func_wrap(
                "wafer",
                "log",
                |caller: Caller<'_, HostState>,
                 level_ptr: i32,
                 level_len: i32,
                 msg_ptr: i32,
                 msg_len: i32| {
                    let level =
                        host::mem_read_caller(&caller, level_ptr as u32, level_len as u32).unwrap_or_default();
                    let msg =
                        host::mem_read_caller(&caller, msg_ptr as u32, msg_len as u32).unwrap_or_default();
                    let level_str = String::from_utf8_lossy(&level);
                    let msg_str = String::from_utf8_lossy(&msg);
                    match level_str.as_ref() {
                        "debug" => tracing::debug!("{}", msg_str),
                        "info" => tracing::info!("{}", msg_str),
                        "warn" => tracing::warn!("{}", msg_str),
                        "error" => tracing::error!("{}", msg_str),
                        _ => tracing::info!("{}", msg_str),
                    }
                },
            )
            .map_err(|e| format!("linking log: {e}"))?;

        // wafer.call_block(name_ptr, name_len, msg_ptr, msg_len) -> i64
        linker
            .func_wrap(
                "wafer",
                "call_block",
                |mut caller: Caller<'_, HostState>,
                 name_ptr: i32,
                 name_len: i32,
                 msg_ptr: i32,
                 msg_len: i32|
                 -> i64 {
                    // Read block name
                    let name_bytes =
                        match host::mem_read_caller(&caller, name_ptr as u32, name_len as u32) {
                            Ok(b) => b,
                            Err(_) => return 0,
                        };
                    let block_name = String::from_utf8_lossy(&name_bytes).to_string();

                    // Read message JSON
                    let msg_bytes =
                        match host::mem_read_caller(&caller, msg_ptr as u32, msg_len as u32) {
                            Ok(b) => b,
                            Err(_) => return 0,
                        };

                    // Deserialize message
                    let mut internal_msg: Message = match serde_json::from_slice(&msg_bytes) {
                        Ok(m) => m,
                        Err(_) => return 0,
                    };

                    // Call through context (sync bridge — wasmi host functions are inherently sync)
                    let result = {
                        let ctx = match &caller.data().context {
                            Some(ctx) => ctx.clone(),
                            None => return 0,
                        };
                        poll_once_sync(ctx.call_block(&block_name, &mut internal_msg))
                    };

                    // Serialize result
                    let result_json = match serde_json::to_vec(&result) {
                        Ok(j) => j,
                        Err(_) => return 0,
                    };

                    // Write result into guest memory
                    match host::mem_write(&mut caller, &result_json) {
                        Ok((ptr, len)) => host::pack_ptr_len(ptr, len),
                        Err(_) => 0,
                    }
                },
            )
            .map_err(|e| format!("linking call_block: {e}"))?;

        Ok(())
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
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

        let (mut store, instance) = match self.create_instance(None) {
            Ok(r) => r,
            Err(e) => return error_block_info(&format!("failed to create instance: {}", e)),
        };

        let info_fn = match instance
            .get_typed_func::<(), i64>(&store, "__wafer_info")
        {
            Ok(f) => f,
            Err(e) => return error_block_info(&format!("missing __wafer_info: {}", e)),
        };

        let packed = match info_fn.call(&mut store, ()) {
            Ok(v) => v,
            Err(e) => return error_block_info(&format!("calling __wafer_info: {}", e)),
        };

        let (ptr, len) = host::unpack_ptr_len(packed);
        let memory = match instance.get_memory(&store, "memory") {
            Some(m) => m,
            None => return error_block_info("guest has no exported memory"),
        };
        let json_bytes = match host::mem_read(&store, &memory, ptr, len) {
            Ok(b) => b,
            Err(e) => return error_block_info(&format!("reading info result: {}", e)),
        };

        let info: BlockInfo = match serde_json::from_slice(&json_bytes) {
            Ok(i) => i,
            Err(e) => return error_block_info(&format!("parsing info JSON: {}", e)),
        };

        // Cache it
        if let Ok(mut guard) = self.info_cache.lock() {
            *guard = Some(info.clone());
        }

        info
    }

    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let guard = ContextGuard::new(ctx);

        let (mut store, instance) = match self.create_instance(Some(guard.as_arc())) {
            Ok(r) => r,
            Err(e) => return msg.clone().err(WaferError::new(ErrorCode::INTERNAL, e)),
        };

        // Serialize message to JSON
        let msg_json = match serde_json::to_vec(msg) {
            Ok(j) => j,
            Err(e) => {
                return msg
                    .clone()
                    .err(WaferError::new(ErrorCode::INTERNAL, format!("serializing message: {e}")));
            }
        };

        // Get guest allocator and handle function
        let alloc_fn = match instance
            .get_typed_func::<i32, i32>(&store, "__wafer_alloc")
        {
            Ok(f) => f,
            Err(e) => {
                return msg
                    .clone()
                    .err(WaferError::new(ErrorCode::INTERNAL, format!("missing __wafer_alloc: {e}")));
            }
        };

        let handle_fn = match instance
            .get_typed_func::<(i32, i32), i64>(&store, "__wafer_handle")
        {
            Ok(f) => f,
            Err(e) => {
                return msg
                    .clone()
                    .err(WaferError::new(ErrorCode::INTERNAL, format!("missing __wafer_handle: {e}")));
            }
        };

        // Allocate and write message into guest memory
        let msg_len = msg_json.len() as i32;
        let msg_ptr = match alloc_fn.call(&mut store, msg_len) {
            Ok(ptr) => ptr,
            Err(e) => {
                return msg
                    .clone()
                    .err(WaferError::new(ErrorCode::INTERNAL, format!("alloc failed: {e}")));
            }
        };

        // Write into guest memory
        let memory = match instance.get_memory(&store, "memory") {
            Some(m) => m,
            None => {
                return msg
                    .clone()
                    .err(WaferError::new(ErrorCode::INTERNAL, "guest has no exported memory"));
            }
        };

        let mem_data = memory.data_mut(&mut store);
        let start = msg_ptr as usize;
        let end = start + msg_json.len();
        if end > mem_data.len() {
            return msg
                .clone()
                .err(WaferError::new(ErrorCode::INTERNAL, "message too large for guest memory"));
        }
        mem_data[start..end].copy_from_slice(&msg_json);

        // Call handle
        let packed = match handle_fn.call(&mut store, (msg_ptr, msg_len)) {
            Ok(v) => v,
            Err(e) => {
                return msg.clone().err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("calling __wafer_handle: {}", e),
                ));
            }
        };

        // Read result
        let (rptr, rlen) = host::unpack_ptr_len(packed);
        let result_bytes = match host::mem_read(&store, &memory, rptr, rlen) {
            Ok(b) => b,
            Err(e) => {
                return msg.clone().err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("reading handle result: {}", e),
                ));
            }
        };

        let mut result: Result_ = match serde_json::from_slice(&result_bytes) {
            Ok(r) => r,
            Err(e) => {
                return msg.clone().err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("parsing handle result: {}", e),
                ));
            }
        };

        if result.message.is_none() {
            result.message = Some(msg.clone());
        }
        result
    }

    async fn lifecycle(
        &self,
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        let guard = ContextGuard::new(ctx);

        let (mut store, instance) = self
            .create_instance(Some(guard.as_arc()))
            .map_err(|e| WaferError::new(ErrorCode::INTERNAL, e))?;

        // Serialize lifecycle event to JSON
        let event_json = serde_json::to_vec(&event)
            .map_err(|e| WaferError::new(ErrorCode::INTERNAL, format!("serializing event: {e}")))?;

        let alloc_fn = instance
            .get_typed_func::<i32, i32>(&store, "__wafer_alloc")
            .map_err(|e| WaferError::new(ErrorCode::INTERNAL, format!("missing __wafer_alloc: {e}")))?;

        let lifecycle_fn = instance
            .get_typed_func::<(i32, i32), i64>(&store, "__wafer_lifecycle")
            .map_err(|e| WaferError::new(ErrorCode::INTERNAL, format!("missing __wafer_lifecycle: {e}")))?;

        // Allocate and write event into guest memory
        let event_len = event_json.len() as i32;
        let event_ptr = alloc_fn
            .call(&mut store, event_len)
            .map_err(|e| WaferError::new(ErrorCode::INTERNAL, format!("alloc failed: {e}")))?;

        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| WaferError::new(ErrorCode::INTERNAL, "guest has no exported memory"))?;

        let mem_data = memory.data_mut(&mut store);
        let start = event_ptr as usize;
        let end = start + event_json.len();
        if end > mem_data.len() {
            return Err(WaferError::new(ErrorCode::INTERNAL, "event too large for guest memory"));
        }
        mem_data[start..end].copy_from_slice(&event_json);

        // Call lifecycle
        let packed = lifecycle_fn
            .call(&mut store, (event_ptr, event_len))
            .map_err(|e| WaferError::new(ErrorCode::INTERNAL, format!("calling __wafer_lifecycle: {e}")))?;

        // Read result (empty JSON = ok, or error JSON)
        let (rptr, rlen) = host::unpack_ptr_len(packed);
        if rlen == 0 {
            return Ok(());
        }

        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| WaferError::new(ErrorCode::INTERNAL, "guest has no exported memory"))?;
        let result_bytes = host::mem_read(&store, &memory, rptr, rlen)
            .map_err(|e| WaferError::new(ErrorCode::INTERNAL, format!("reading lifecycle result: {e}")))?;

        // Try to parse as error
        if let Ok(err) = serde_json::from_slice::<WaferError>(&result_bytes) {
            if !err.message.is_empty() {
                return Err(err);
            }
        }

        Ok(())
    }
}

fn error_block_info(summary: &str) -> BlockInfo {
    BlockInfo {
        name: "unknown".to_string(),
        version: "0.0.0".to_string(),
        interface: "error".to_string(),
        summary: summary.to_string(),
        instance_mode: InstanceMode::PerNode,
        allowed_modes: Vec::new(),
        admin_ui: None,
        runtime: BlockRuntime::Wasm,
        requires: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// ContextWrapper: wrap a &dyn Context as an Arc<dyn Context>
// ---------------------------------------------------------------------------

struct ContextWrapper(*const dyn Context);
unsafe impl Send for ContextWrapper {}
unsafe impl Sync for ContextWrapper {}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
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

    fn registered_blocks(&self) -> Vec<crate::block::BlockInfo> {
        unsafe { &*self.0 }.registered_blocks()
    }

    fn flow_infos(&self) -> Vec<crate::config::FlowInfo> {
        unsafe { &*self.0 }.flow_infos()
    }

    fn flow_defs(&self) -> Vec<crate::config::FlowDef> {
        unsafe { &*self.0 }.flow_defs()
    }
}

/// RAII guard that ensures the ContextWrapper (and any Arc clones of it) cannot
/// outlive the borrowed `&dyn Context`.
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

impl Drop for ContextGuard {
    fn drop(&mut self) {
        let count = Arc::strong_count(&self.arc);
        if count != 1 {
            panic!(
                "ContextGuard: Arc leaked with {} references — potential use-after-free bug",
                count
            );
        }
    }
}
