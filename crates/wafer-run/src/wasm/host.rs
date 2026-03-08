use std::sync::Arc;

use wasmi::{Caller, Memory, AsContext, AsContextMut, Val};

use crate::context::Context;
use crate::wasm::capabilities::BlockCapabilities;

/// HostState stores the wafer Context and capabilities for host function calls.
pub struct HostState {
    pub context: Option<Arc<dyn Context>>,
    pub capabilities: BlockCapabilities,
}

// ---------------------------------------------------------------------------
// Memory helpers for thin ABI
// ---------------------------------------------------------------------------

/// Read a byte slice from WASM linear memory at the given pointer and length.
pub fn mem_read(ctx: impl AsContext<Data = HostState>, memory: &Memory, ptr: u32, len: u32) -> Result<Vec<u8>, String> {
    let data = memory.data(&ctx);
    let start = ptr as usize;
    let end = start + len as usize;
    if end > data.len() {
        return Err(format!(
            "out of bounds memory read: {}..{} (memory size {})",
            start,
            end,
            data.len()
        ));
    }
    Ok(data[start..end].to_vec())
}

/// Read a byte slice from WASM linear memory using a Caller (for use in host functions).
pub fn mem_read_caller(caller: &Caller<'_, HostState>, ptr: u32, len: u32) -> Result<Vec<u8>, String> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| "guest has no exported memory".to_string())?;

    let data = memory.data(caller);
    let start = ptr as usize;
    let end = start + len as usize;
    if end > data.len() {
        return Err(format!(
            "out of bounds memory read: {}..{} (memory size {})",
            start,
            end,
            data.len()
        ));
    }
    Ok(data[start..end].to_vec())
}

/// Write bytes into WASM linear memory using the guest's allocator.
///
/// Returns `(ptr, len)` of the written data in guest memory.
/// The guest must export `__wafer_alloc(len: i32) -> i32`.
pub fn mem_write(caller: &mut Caller<'_, HostState>, data: &[u8]) -> Result<(u32, u32), String> {
    let alloc = caller
        .get_export("__wafer_alloc")
        .and_then(|e| e.into_func())
        .ok_or_else(|| "guest has no __wafer_alloc export".to_string())?;

    let len = data.len() as i32;
    let mut result = [Val::I32(0)];
    alloc
        .call(caller.as_context_mut(), &[Val::I32(len)], &mut result)
        .map_err(|e| format!("calling __wafer_alloc: {e}"))?;

    let ptr = match result[0] {
        Val::I32(v) => v as u32,
        _ => return Err("__wafer_alloc returned non-i32".to_string()),
    };

    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| "guest has no exported memory".to_string())?;

    let mem_data = memory.data_mut(caller.as_context_mut());
    let start = ptr as usize;
    let end = start + data.len();
    if end > mem_data.len() {
        return Err(format!(
            "out of bounds memory write: {}..{} (memory size {})",
            start,
            end,
            mem_data.len()
        ));
    }
    mem_data[start..end].copy_from_slice(data);

    Ok((ptr, data.len() as u32))
}

/// Pack a (ptr, len) pair into a single i64 return value.
/// High 32 bits = ptr, low 32 bits = len.
pub fn pack_ptr_len(ptr: u32, len: u32) -> i64 {
    ((ptr as i64) << 32) | (len as i64)
}

/// Unpack a packed i64 into (ptr, len).
pub fn unpack_ptr_len(packed: i64) -> (u32, u32) {
    let ptr = (packed >> 32) as u32;
    let len = (packed & 0xFFFF_FFFF) as u32;
    (ptr, len)
}
