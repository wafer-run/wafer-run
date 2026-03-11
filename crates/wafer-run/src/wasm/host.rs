use std::sync::Arc;

use wasmi::{Caller, Memory, AsContext};

use crate::context::Context;
use crate::types::Message;
use crate::wasm::capabilities::BlockCapabilities;

/// Pending call_block request stored by the host function before trapping.
pub struct PendingCall {
    pub block_name: String,
    pub message: Message,
}

/// HostState stores the wafer Context and capabilities for host function calls.
pub struct HostState {
    pub context: Option<Arc<dyn Context>>,
    pub capabilities: BlockCapabilities,
    /// Pending call_block request — set by the call_block host function before trapping.
    pub pending_call: Option<PendingCall>,
    /// Result of the last call_block — stored by the resumable loop, read by read_result.
    pub pending_result: Option<Vec<u8>>,
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
