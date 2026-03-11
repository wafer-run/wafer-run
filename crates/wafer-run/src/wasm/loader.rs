use std::sync::{Arc, Mutex};

use wasmi::{Store, Engine, Module, Config, Linker, Caller, Instance, Val, ResumableCall};

use crate::block::{Block, BlockInfo};
use crate::common::ErrorCode;
use crate::context::Context;
use crate::types::*;
use super::capabilities::BlockCapabilities;
use super::host::{self, HostState, PendingCall};

/// Marker error used to trap WASM execution when call_block needs async work.
/// The actual request data is stored in HostState.pending_call.
#[derive(Debug)]
struct CallBlockTrap;

impl std::fmt::Display for CallBlockTrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("call_block")
    }
}

impl wasmi::core::HostError for CallBlockTrap {}

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
/// - `wafer.call_block(name_ptr: i32, name_len: i32, msg_ptr: i32, msg_len: i32) -> i32` — call another block (traps for async, returns result_len on resume)
/// - `wafer.read_result(dest_ptr: i32, dest_len: i32) -> i32` — copy last call_block result into guest memory
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
                pending_call: None,
                pending_result: None,
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

        // wafer.call_block(name_ptr, name_len, msg_ptr, msg_len) -> i32
        //
        // Traps to signal the host that an async call_block is needed.
        // The request is stored in HostState.pending_call before trapping.
        // After the host performs the async call and stores the result in
        // HostState.pending_result, it resumes WASM with the result length.
        linker
            .func_wrap(
                "wafer",
                "call_block",
                |mut caller: Caller<'_, HostState>,
                 name_ptr: i32,
                 name_len: i32,
                 msg_ptr: i32,
                 msg_len: i32|
                 -> Result<i32, wasmi::Error> {
                    // Read block name
                    let name_bytes =
                        host::mem_read_caller(&caller, name_ptr as u32, name_len as u32)
                            .map_err(|e| wasmi::Error::new(format!("reading block name: {e}")))?;
                    let block_name = String::from_utf8_lossy(&name_bytes).to_string();

                    // Read message JSON
                    let msg_bytes =
                        host::mem_read_caller(&caller, msg_ptr as u32, msg_len as u32)
                            .map_err(|e| wasmi::Error::new(format!("reading message: {e}")))?;

                    // Deserialize message
                    let message: Message = serde_json::from_slice(&msg_bytes)
                        .map_err(|e| wasmi::Error::new(format!("deserializing message: {e}")))?;

                    // Store the request in host state
                    caller.data_mut().pending_call = Some(PendingCall {
                        block_name,
                        message,
                    });

                    // Trap with a host error — the resumable loop will handle this async
                    Err(wasmi::Error::host(CallBlockTrap))
                },
            )
            .map_err(|e| format!("linking call_block: {e}"))?;

        // wafer.read_result(dest_ptr, dest_len) -> i32
        //
        // Copies the result of the last call_block from host memory into
        // guest memory at dest_ptr. Returns the number of bytes copied.
        linker
            .func_wrap(
                "wafer",
                "read_result",
                |mut caller: Caller<'_, HostState>,
                 dest_ptr: i32,
                 dest_len: i32|
                 -> i32 {
                    let result_bytes = match caller.data_mut().pending_result.take() {
                        Some(b) => b,
                        None => return 0,
                    };

                    let copy_len = std::cmp::min(result_bytes.len(), dest_len as usize);

                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => return 0,
                    };

                    let mem_data = memory.data_mut(&mut caller);
                    let start = dest_ptr as usize;
                    let end = start + copy_len;
                    if end > mem_data.len() {
                        return 0;
                    }
                    mem_data[start..end].copy_from_slice(&result_bytes[..copy_len]);

                    copy_len as i32
                },
            )
            .map_err(|e| format!("linking read_result: {e}"))?;

        Ok(())
    }

    /// Drive a resumable WASM call, handling call_block traps by awaiting
    /// the async operation and resuming WASM execution with the result.
    async fn drive_resumable(
        store: &mut Store<HostState>,
        mut call: ResumableCall,
        results: &mut [Val],
    ) -> Result<(), String> {
        loop {
            match call {
                ResumableCall::Finished => return Ok(()),
                ResumableCall::Resumable(invocation) => {
                    // Take the pending call request from host state
                    let pending = store
                        .data_mut()
                        .pending_call
                        .take()
                        .ok_or_else(|| "resumable trap without pending_call".to_string())?;

                    // Perform the actual async call_block
                    let ctx = store
                        .data()
                        .context
                        .as_ref()
                        .ok_or_else(|| "no context for call_block".to_string())?
                        .clone();

                    let mut msg = pending.message;
                    let result = ctx.call_block(&pending.block_name, &mut msg).await;

                    // Serialize and store the result for read_result
                    let result_bytes = serde_json::to_vec(&result)
                        .map_err(|e| format!("serializing call_block result: {e}"))?;
                    let result_len = result_bytes.len() as i32;
                    store.data_mut().pending_result = Some(result_bytes);

                    // Resume WASM with the result length as the return value of call_block
                    call = invocation
                        .resume(&mut *store, &[Val::I32(result_len)], results)
                        .map_err(|e| format!("resuming WASM after call_block: {e}"))?;
                }
            }
        }
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

        // Get the handle function (untyped Func for resumable call support)
        let handle_fn = match instance.get_func(&store, "__wafer_handle") {
            Some(f) => f,
            None => {
                return msg
                    .clone()
                    .err(WaferError::new(ErrorCode::INTERNAL, "missing __wafer_handle"));
            }
        };

        // Allocate and write message into guest memory
        let alloc_fn = match instance.get_typed_func::<i32, i32>(&store, "__wafer_alloc") {
            Ok(f) => f,
            Err(e) => {
                return msg
                    .clone()
                    .err(WaferError::new(ErrorCode::INTERNAL, format!("missing __wafer_alloc: {e}")));
            }
        };

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

        // Call handle using resumable call
        let mut results = [Val::I64(0)];
        let call = match handle_fn.call_resumable(&mut store, &[Val::I32(msg_ptr), Val::I32(msg_len)], &mut results) {
            Ok(c) => c,
            Err(e) => {
                return msg.clone().err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("calling __wafer_handle: {}", e),
                ));
            }
        };

        // Drive the resumable call loop (handles call_block traps)
        if let Err(e) = Self::drive_resumable(&mut store, call, &mut results).await {
            return msg.clone().err(WaferError::new(ErrorCode::INTERNAL, e));
        }

        // Read result from the packed i64 return value
        let packed = match results[0] {
            Val::I64(v) => v,
            _ => {
                return msg.clone().err(WaferError::new(
                    ErrorCode::INTERNAL,
                    "__wafer_handle returned non-i64",
                ));
            }
        };

        let (rptr, rlen) = host::unpack_ptr_len(packed);
        let memory = match instance.get_memory(&store, "memory") {
            Some(m) => m,
            None => {
                return msg.clone().err(WaferError::new(
                    ErrorCode::INTERNAL,
                    "guest has no exported memory",
                ));
            }
        };
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
            .get_func(&store, "__wafer_lifecycle")
            .ok_or_else(|| WaferError::new(ErrorCode::INTERNAL, "missing __wafer_lifecycle"))?;

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

        // Call lifecycle using resumable call
        let mut results = [Val::I64(0)];
        let call = lifecycle_fn
            .call_resumable(&mut store, &[Val::I32(event_ptr), Val::I32(event_len)], &mut results)
            .map_err(|e| WaferError::new(ErrorCode::INTERNAL, format!("calling __wafer_lifecycle: {e}")))?;

        // Drive the resumable call loop
        Self::drive_resumable(&mut store, call, &mut results)
            .await
            .map_err(|e| WaferError::new(ErrorCode::INTERNAL, e))?;

        // Read result (empty JSON = ok, or error JSON)
        let packed = match results[0] {
            Val::I64(v) => v,
            _ => return Ok(()),
        };
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
