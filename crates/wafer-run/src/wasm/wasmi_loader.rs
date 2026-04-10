use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tracing::{debug, warn};
use wasmi::{Caller, Engine, Error as WasmiError, Linker, Module, Store, TypedResumableCall, Val};

use crate::block::{Block, BlockInfo};
use crate::context::Context;
use crate::types::*;

use super::capabilities::BlockCapabilities;
use super::host::ContextGuard;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default fuel budget for a single guest invocation.
const DEFAULT_FUEL: u64 = 100_000_000;

/// Maximum WASM linear-memory pages (256 pages = 16 MiB).
const MAX_WASM_MEMORY_PAGES: usize = 256;

// ---------------------------------------------------------------------------
// Packed pointer helpers
// ---------------------------------------------------------------------------

/// Pack a (ptr, len) pair into a single i64 for the ABI.
fn pack_ptr_len(ptr: u32, len: u32) -> i64 {
    ((ptr as i64) << 32) | (len as i64)
}

/// Unpack a packed i64 into (ptr, len).
fn unpack_ptr_len(packed: i64) -> (u32, u32) {
    let ptr = (packed >> 32) as u32;
    let len = (packed & 0xFFFF_FFFF) as u32;
    (ptr, len)
}

// ---------------------------------------------------------------------------
// Host state stored in the wasmi Store
// ---------------------------------------------------------------------------

struct WasmiHostState {
    /// Context reference — set before each guest call via ContextGuard.
    context: Option<Arc<dyn Context>>,
    /// Capabilities (resource limits) for this block.
    /// Stored for future use by host function enforcement.
    #[allow(dead_code)]
    capabilities: BlockCapabilities,
    /// Set by __wafer_host_call_block to request an async call.
    pending_call: Option<PendingCall>,
    /// Set by the host after resolving a pending_call; the guest reads this
    /// on the resumed/replayed invocation.
    pending_result: Option<Vec<u8>>,
}

struct PendingCall {
    block_name: String,
    msg_bytes: Vec<u8>,
}

impl wasmi::ResourceLimiter for WasmiHostState {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmi::errors::MemoryError> {
        // One WASM page = 64 KiB.
        let desired_pages = desired / 65536;
        Ok(desired_pages <= MAX_WASM_MEMORY_PAGES)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmi::errors::TableError> {
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Sentinel error for call_block trap+resume
// ---------------------------------------------------------------------------

/// Marker error returned by `__wafer_host_call_block` to suspend execution.
/// wasmi's resumable-call machinery catches this and lets the host resolve
/// the async call before resuming.
#[derive(Debug)]
struct CallBlockTrap;

impl std::fmt::Display for CallBlockTrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "call_block trap (expected — will be resumed)")
    }
}

impl wasmi::core::HostError for CallBlockTrap {}

// ---------------------------------------------------------------------------
// Guest memory helpers
// ---------------------------------------------------------------------------

/// Read `len` bytes starting at `offset` from the guest's exported `memory`.
fn read_guest_bytes(
    store: &Store<WasmiHostState>,
    memory: wasmi::Memory,
    offset: u32,
    len: u32,
) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; len as usize];
    memory
        .read(store, offset as usize, &mut buf)
        .map_err(|e| format!("reading guest memory at {offset}+{len}: {e}"))?;
    Ok(buf)
}

/// Allocate space in guest memory via `__wafer_alloc`, then write `data`.
/// Returns the guest pointer.
fn write_guest_bytes(
    store: &mut Store<WasmiHostState>,
    alloc_fn: wasmi::TypedFunc<i32, i32>,
    memory: wasmi::Memory,
    data: &[u8],
) -> Result<u32, String> {
    let len = data.len() as i32;
    let ptr = alloc_fn
        .call(&mut *store, len)
        .map_err(|e| format!("__wafer_alloc({len}): {e}"))?;
    memory
        .write(&mut *store, ptr as usize, data)
        .map_err(|e| format!("writing {len} bytes at guest ptr {ptr}: {e}"))?;
    Ok(ptr as u32)
}

// ---------------------------------------------------------------------------
// Linker setup
// ---------------------------------------------------------------------------

fn build_linker(engine: &Engine) -> Result<Linker<WasmiHostState>, String> {
    let mut linker = Linker::<WasmiHostState>::new(engine);

    // ---- wafer module: host imports ----

    // __wafer_host_is_cancelled() -> i32
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_is_cancelled",
            |caller: Caller<WasmiHostState>| -> i32 {
                let state = caller.data();
                if let Some(ref ctx) = state.context {
                    if ctx.is_cancelled() {
                        1
                    } else {
                        0
                    }
                } else {
                    0
                }
            },
        )
        .map_err(|e| format!("linking __wafer_host_is_cancelled: {e}"))?;

    // __wafer_host_log(level_ptr, level_len, msg_ptr, msg_len)
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_log",
            |caller: Caller<WasmiHostState>,
             level_ptr: i32,
             level_len: i32,
             msg_ptr: i32,
             msg_len: i32| {
                let memory = match caller.get_export("memory") {
                    Some(wasmi::Extern::Memory(m)) => m,
                    _ => return,
                };
                let level_bytes = {
                    let mut buf = vec![0u8; level_len as usize];
                    if memory.read(&caller, level_ptr as usize, &mut buf).is_err() {
                        return;
                    }
                    buf
                };
                let msg_bytes = {
                    let mut buf = vec![0u8; msg_len as usize];
                    if memory.read(&caller, msg_ptr as usize, &mut buf).is_err() {
                        return;
                    }
                    buf
                };
                let level = String::from_utf8_lossy(&level_bytes);
                let msg = String::from_utf8_lossy(&msg_bytes);
                match level.as_ref() {
                    "error" => tracing::error!(target: "wasm_guest", "{msg}"),
                    "warn" => tracing::warn!(target: "wasm_guest", "{msg}"),
                    "debug" => tracing::debug!(target: "wasm_guest", "{msg}"),
                    "trace" => tracing::trace!(target: "wasm_guest", "{msg}"),
                    _ => tracing::info!(target: "wasm_guest", "{msg}"),
                }
            },
        )
        .map_err(|e| format!("linking __wafer_host_log: {e}"))?;

    // __wafer_host_call_block(name_ptr, name_len, msg_ptr, msg_len) -> i64
    //
    // Two-phase protocol:
    //   Phase 1 (no pending_result): read args, store PendingCall, trap.
    //   Phase 2 (pending_result set): write result into guest memory, return packed ptr.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_call_block",
            |mut caller: Caller<WasmiHostState>,
             name_ptr: i32,
             name_len: i32,
             msg_ptr: i32,
             msg_len: i32|
             -> Result<i64, WasmiError> {
                // Phase 2: if we already have a result from a previous resolution,
                // return it to the guest.
                if let Some(result_bytes) = caller.data_mut().pending_result.take() {
                    let memory = caller
                        .get_export("memory")
                        .and_then(|e| e.into_memory())
                        .ok_or_else(|| WasmiError::new("guest has no exported memory"))?;
                    // Use __wafer_alloc to allocate in guest memory.
                    let alloc_fn = caller
                        .get_export("__wafer_alloc")
                        .and_then(|e| e.into_func())
                        .ok_or_else(|| WasmiError::new("guest has no __wafer_alloc export"))?;
                    let len = result_bytes.len() as i32;
                    let mut alloc_result = [Val::I32(0)];
                    alloc_fn
                        .call(&mut caller, &[Val::I32(len)], &mut alloc_result)
                        .map_err(|e| {
                            WasmiError::new(format!("__wafer_alloc in call_block phase 2: {e}"))
                        })?;
                    let ptr = match alloc_result[0] {
                        Val::I32(v) => v,
                        _ => return Err(WasmiError::new("__wafer_alloc returned non-i32")),
                    };
                    memory
                        .write(&mut caller, ptr as usize, &result_bytes)
                        .map_err(|e| WasmiError::new(format!("writing call_block result: {e}")))?;
                    return Ok(pack_ptr_len(ptr as u32, len as u32));
                }

                // Phase 1: read block name and message from guest memory, then trap.
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| WasmiError::new("guest has no exported memory"))?;

                let mut name_buf = vec![0u8; name_len as usize];
                memory
                    .read(&caller, name_ptr as usize, &mut name_buf)
                    .map_err(|e| WasmiError::new(format!("reading block name: {e}")))?;
                let block_name = String::from_utf8(name_buf)
                    .map_err(|e| WasmiError::new(format!("block name not UTF-8: {e}")))?;

                // Capability check: deny if block is not allowed to call the target.
                if !caller.data().capabilities.allows_call_block(&block_name) {
                    return Err(WasmiError::new(format!(
                        "call_block to '{}' denied by block capabilities",
                        block_name
                    )));
                }

                let mut msg_buf = vec![0u8; msg_len as usize];
                memory
                    .read(&caller, msg_ptr as usize, &mut msg_buf)
                    .map_err(|e| WasmiError::new(format!("reading call_block message: {e}")))?;

                // Store the pending call and trap to yield control.
                caller.data_mut().pending_call = Some(PendingCall {
                    block_name,
                    msg_bytes: msg_buf,
                });
                Err(WasmiError::host(CallBlockTrap))
            },
        )
        .map_err(|e| format!("linking __wafer_host_call_block: {e}"))?;

    // ---- WASI stubs (wasi_snapshot_preview1) ----

    // fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            |mut caller: Caller<WasmiHostState>,
             _fd: i32,
             _iovs_ptr: i32,
             _iovs_len: i32,
             nwritten_ptr: i32|
             -> i32 {
                // Discard output. Write 0 to nwritten.
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, nwritten_ptr as usize, &0u32.to_le_bytes());
                }
                0 // __WASI_ERRNO_SUCCESS
            },
        )
        .map_err(|e| format!("linking fd_write stub: {e}"))?;

    // proc_exit(code)
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "proc_exit",
            |_caller: Caller<WasmiHostState>, code: i32| -> Result<(), WasmiError> {
                Err(WasmiError::new(format!("guest called proc_exit({code})")))
            },
        )
        .map_err(|e| format!("linking proc_exit stub: {e}"))?;

    // environ_sizes_get(argc_ptr, argv_buf_size_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_sizes_get",
            |mut caller: Caller<WasmiHostState>, argc_ptr: i32, argv_buf_size_ptr: i32| -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, argc_ptr as usize, &0u32.to_le_bytes());
                    let _ =
                        memory.write(&mut caller, argv_buf_size_ptr as usize, &0u32.to_le_bytes());
                }
                0
            },
        )
        .map_err(|e| format!("linking environ_sizes_get stub: {e}"))?;

    // environ_get(argv_ptr, argv_buf_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_get",
            |_caller: Caller<WasmiHostState>, _argv_ptr: i32, _argv_buf_ptr: i32| -> i32 { 0 },
        )
        .map_err(|e| format!("linking environ_get stub: {e}"))?;

    Ok(linker)
}

// ---------------------------------------------------------------------------
// Instantiation helper
// ---------------------------------------------------------------------------

/// Create a fresh store + instance from the pre-built linker and module.
fn instantiate(
    engine: &Engine,
    linker: &Linker<WasmiHostState>,
    module: &Module,
    caps: &BlockCapabilities,
) -> Result<(Store<WasmiHostState>, wasmi::Instance), String> {
    let host_state = WasmiHostState {
        context: None,
        capabilities: caps.clone(),
        pending_call: None,
        pending_result: None,
    };
    let mut store = Store::new(engine, host_state);

    // Resource limits.
    store.limiter(|state| state);
    store
        .set_fuel(DEFAULT_FUEL)
        .map_err(|e| format!("setting fuel: {e}"))?;

    let pre = linker
        .instantiate(&mut store, module)
        .map_err(|e| format!("instantiation: {e}"))?;
    let instance = pre
        .start(&mut store)
        .map_err(|e| format!("running start function: {e}"))?;

    Ok((store, instance))
}

// ---------------------------------------------------------------------------
// WasmiBlock
// ---------------------------------------------------------------------------

pub struct WasmiBlock {
    engine: Engine,
    module: Module,
    linker: Linker<WasmiHostState>,
    info_cache: Mutex<Option<BlockInfo>>,
    capabilities: BlockCapabilities,
}

// Safety: Engine, Module, Linker are Send+Sync in wasmi 0.44.
// The Mutex guards the cache.
unsafe impl Send for WasmiBlock {}
unsafe impl Sync for WasmiBlock {}

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
        let mut config = wasmi::Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        Self::load_with_engine(&engine, wasm_bytes, caps)
    }

    pub fn load_with_engine(
        engine: &Engine,
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
    ) -> Result<Self, String> {
        let module =
            Module::new(engine, wasm_bytes).map_err(|e| format!("compiling WASM module: {e}"))?;
        let linker = build_linker(engine)?;
        Ok(Self {
            engine: engine.clone(),
            module,
            linker,
            info_cache: Mutex::new(None),
            capabilities: caps,
        })
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Call a guest function that returns a packed i64 (ptr << 32 | len),
    /// handling the call_block trap+resume loop.
    ///
    /// `setup` prepares the store/instance (writes args, returns the TypedFunc).
    /// When a `call_block` trap occurs the loop resolves it via `ctx` and resumes.
    async fn call_guest_resumable(
        &self,
        ctx: &dyn Context,
        setup: impl FnOnce(
            &mut Store<WasmiHostState>,
            wasmi::Instance,
        ) -> Result<(wasmi::TypedFunc<(i32, i32), i64>, i32, i32), String>,
    ) -> Result<Vec<u8>, String> {
        let guard = ContextGuard::new(ctx);
        let (mut store, instance) =
            instantiate(&self.engine, &self.linker, &self.module, &self.capabilities)?;
        store.data_mut().context = Some(guard.as_arc());

        let (func, arg0, arg1) = setup(&mut store, instance)?;

        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| "guest has no exported memory".to_string())?;

        // Initial call (resumable).
        let mut resumable = match func
            .call_resumable(&mut store, (arg0, arg1))
            .map_err(|e| format!("guest call failed: {e}"))?
        {
            TypedResumableCall::Finished(packed) => {
                let (ptr, len) = unpack_ptr_len(packed);
                let result = read_guest_bytes(&store, memory, ptr, len)?;
                // Drop context ref before guard.
                store.data_mut().context = None;
                return Ok(result);
            }
            TypedResumableCall::Resumable(inv) => inv,
        };

        // Resolve pending calls in a loop.
        loop {
            let pending = store.data_mut().pending_call.take().ok_or_else(|| {
                format!(
                    "guest trapped but no pending_call (host error: {})",
                    resumable.host_error()
                )
            })?;

            debug!(
                block = pending.block_name,
                msg_len = pending.msg_bytes.len(),
                "resolving call_block from WASM guest"
            );

            // Deserialize the message, call the block, serialize the result.
            let mut msg: Message = serde_json::from_slice(&pending.msg_bytes)
                .map_err(|e| format!("deserializing call_block message: {e}"))?;

            let result = ctx.call_block(&pending.block_name, &mut msg).await;
            let result_bytes = serde_json::to_vec(&result)
                .map_err(|e| format!("serializing call_block result: {e}"))?;

            // Provide the result for phase 2.
            store.data_mut().pending_result = Some(result_bytes);

            // Resume. The host func (__wafer_host_call_block) returns i64,
            // so we must provide a single i64 Val. However, the resumption
            // re-enters the host function which checks pending_result, so
            // the value we pass here is not directly used — the host func
            // will overwrite it. We pass 0 as a placeholder; wasmi requires
            // the correct number/type of resume inputs matching the host
            // function's return type.
            match resumable
                .resume(&mut store, &[Val::I64(0)])
                .map_err(|e| format!("resuming guest after call_block: {e}"))?
            {
                TypedResumableCall::Finished(packed) => {
                    let (ptr, len) = unpack_ptr_len(packed);
                    let result = read_guest_bytes(&store, memory, ptr, len)?;
                    store.data_mut().context = None;
                    return Ok(result);
                }
                TypedResumableCall::Resumable(next) => {
                    resumable = next;
                    // Continue the loop: another call_block from the same invocation.
                }
            }
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl Block for WasmiBlock {
    fn info(&self) -> BlockInfo {
        // Check cache first.
        if let Ok(guard) = self.info_cache.lock() {
            if let Some(ref info) = *guard {
                return info.clone();
            }
        }

        // Sync instantiation: call __wafer_info.
        let result = (|| -> Result<BlockInfo, String> {
            let (mut store, instance) =
                instantiate(&self.engine, &self.linker, &self.module, &self.capabilities)?;

            let info_fn = instance
                .get_typed_func::<(), i64>(&store, "__wafer_info")
                .map_err(|e| format!("getting __wafer_info: {e}"))?;

            let memory = instance
                .get_memory(&store, "memory")
                .ok_or_else(|| "guest has no exported memory".to_string())?;

            let packed = info_fn
                .call(&mut store, ())
                .map_err(|e| format!("calling __wafer_info: {e}"))?;

            let (ptr, len) = unpack_ptr_len(packed);
            let bytes = read_guest_bytes(&store, memory, ptr, len)?;
            let info: BlockInfo = serde_json::from_slice(&bytes)
                .map_err(|e| format!("deserializing BlockInfo: {e}"))?;
            Ok(info)
        })();

        match result {
            Ok(info) => {
                if let Ok(mut guard) = self.info_cache.lock() {
                    *guard = Some(info.clone());
                }
                info
            }
            Err(e) => {
                warn!("WasmiBlock::info() failed: {e}");
                BlockInfo::new("unknown", "0.0.0", "unknown", "failed to load info")
            }
        }
    }

    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let msg_bytes = match serde_json::to_vec(msg) {
            Ok(b) => b,
            Err(e) => {
                return Result_ {
                    action: Action::Error,
                    response: None,
                    error: Some(WaferError::new(
                        ErrorCode::Internal,
                        format!("serializing message: {e}"),
                    )),
                    message: None,
                };
            }
        };

        let result_bytes = match self
            .call_guest_resumable(ctx, |store, instance| {
                let alloc_fn = instance
                    .get_typed_func::<i32, i32>(&*store, "__wafer_alloc")
                    .map_err(|e| format!("getting __wafer_alloc: {e}"))?;
                let handle_fn = instance
                    .get_typed_func::<(i32, i32), i64>(&*store, "__wafer_handle")
                    .map_err(|e| format!("getting __wafer_handle: {e}"))?;
                let memory = instance
                    .get_memory(&*store, "memory")
                    .ok_or_else(|| "guest has no exported memory".to_string())?;

                let ptr = write_guest_bytes(store, alloc_fn, memory, &msg_bytes)?;
                let len = msg_bytes.len() as i32;
                Ok((handle_fn, ptr as i32, len))
            })
            .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                return Result_ {
                    action: Action::Error,
                    response: None,
                    error: Some(WaferError::new(
                        ErrorCode::Internal,
                        format!("WASM handle error: {e}"),
                    )),
                    message: None,
                };
            }
        };

        match serde_json::from_slice::<Result_>(&result_bytes) {
            Ok(result) => {
                // Update the caller's message if the guest returned one.
                if let Some(ref new_msg) = result.message {
                    *msg = new_msg.clone();
                }
                result
            }
            Err(e) => Result_ {
                action: Action::Error,
                response: None,
                error: Some(WaferError::new(
                    ErrorCode::Internal,
                    format!("deserializing WASM handle result: {e}"),
                )),
                message: None,
            },
        }
    }

    async fn lifecycle(
        &self,
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        let event_bytes = serde_json::to_vec(&event).map_err(|e| {
            WaferError::new(
                ErrorCode::Internal,
                format!("serializing lifecycle event: {e}"),
            )
        })?;

        let result_bytes = self
            .call_guest_resumable(ctx, |store, instance| {
                let alloc_fn = instance
                    .get_typed_func::<i32, i32>(&*store, "__wafer_alloc")
                    .map_err(|e| format!("getting __wafer_alloc: {e}"))?;
                let lifecycle_fn = instance
                    .get_typed_func::<(i32, i32), i64>(&*store, "__wafer_lifecycle")
                    .map_err(|e| format!("getting __wafer_lifecycle: {e}"))?;
                let memory = instance
                    .get_memory(&*store, "memory")
                    .ok_or_else(|| "guest has no exported memory".to_string())?;

                let ptr = write_guest_bytes(store, alloc_fn, memory, &event_bytes)?;
                let len = event_bytes.len() as i32;
                Ok((lifecycle_fn, ptr as i32, len))
            })
            .await
            .map_err(|e| {
                WaferError::new(ErrorCode::Internal, format!("WASM lifecycle error: {e}"))
            })?;

        // The guest returns a JSON-encoded Result<(), WaferError>.
        let result: std::result::Result<(), WaferError> = serde_json::from_slice(&result_bytes)
            .map_err(|e| {
                WaferError::new(
                    ErrorCode::Internal,
                    format!("deserializing WASM lifecycle result: {e}"),
                )
            })?;
        result
    }

    fn block_capabilities(&self) -> Option<&BlockCapabilities> {
        Some(&self.capabilities)
    }
}
