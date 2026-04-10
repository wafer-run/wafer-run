//! Core ABI for WASM blocks running on wasmi.
//!
//! Provides:
//! - Pack/unpack helpers for the `i64` `(ptr << 32 | len)` pointer convention.
//! - The `__wafer_alloc` export so the host can allocate guest memory.
//! - Safe wrappers around the host imports (`call_block`, `log`, `is_cancelled`).
//!
//! The `extern "C"` FFI declarations and the `__wafer_alloc` export are only
//! meaningful on `wasm32` targets. On all other targets the public wrappers
//! compile to stub functions that panic, ensuring block code that calls them
//! fails loudly rather than silently doing nothing.

use wafer_block::{BlockResult, Message};

// ---------------------------------------------------------------------------
// Pack / unpack helpers (target-independent)
// ---------------------------------------------------------------------------

/// Pack a `(ptr, len)` pair into a single `i64` (`ptr << 32 | len`).
pub fn pack_ptr_len(ptr: u32, len: u32) -> i64 {
    ((ptr as i64) << 32) | (len as i64)
}

/// Unpack an `i64` produced by [`pack_ptr_len`] back into `(ptr, len)`.
pub fn unpack_ptr_len(packed: i64) -> (u32, u32) {
    let ptr = (packed >> 32) as u32;
    let len = (packed & 0xFFFF_FFFF) as u32;
    (ptr, len)
}

// ---------------------------------------------------------------------------
// WASM-only: allocator export + host import FFI
// ---------------------------------------------------------------------------

/// Guest allocator — the host calls this to allocate a buffer in guest memory
/// before writing data that will be passed to the guest.
///
/// The caller is responsible for freeing the allocation (typically by
/// reconstructing the `Vec<u8>` from `ptr`/`capacity` and letting it drop).
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn __wafer_alloc(size: i32) -> i32 {
    let mut buf = Vec::<u8>::with_capacity(size as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr as i32
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "wafer")]
extern "C" {
    fn __wafer_host_is_cancelled() -> i32;
    fn __wafer_host_log(level_ptr: i32, level_len: i32, msg_ptr: i32, msg_len: i32);
    fn __wafer_host_call_block(name_ptr: i32, name_len: i32, msg_ptr: i32, msg_len: i32) -> i64;
}

// ---------------------------------------------------------------------------
// Safe wrappers — wasm32 target
// ---------------------------------------------------------------------------

/// Returns `true` if the current execution has been cancelled by the runtime.
#[cfg(target_arch = "wasm32")]
pub fn is_cancelled() -> bool {
    unsafe { __wafer_host_is_cancelled() != 0 }
}

/// Emit a log message at the given level (e.g. `"info"`, `"warn"`, `"error"`).
#[cfg(target_arch = "wasm32")]
pub fn log(level: &str, msg: &str) {
    unsafe {
        __wafer_host_log(
            level.as_ptr() as i32,
            level.len() as i32,
            msg.as_ptr() as i32,
            msg.len() as i32,
        );
    }
}

/// Call another block by name, passing a [`Message`] and returning the
/// [`BlockResult`] produced by that block.
#[cfg(target_arch = "wasm32")]
pub fn call_block(name: &str, msg: &Message) -> BlockResult {
    let msg_bytes = serde_json::to_vec(msg).expect("failed to serialize message");
    unsafe {
        let packed = __wafer_host_call_block(
            name.as_ptr() as i32,
            name.len() as i32,
            msg_bytes.as_ptr() as i32,
            msg_bytes.len() as i32,
        );
        let (ptr, len) = unpack_ptr_len(packed);
        let bytes = std::slice::from_raw_parts(ptr as *const u8, len as usize);
        serde_json::from_slice(bytes).expect("failed to deserialize BlockResult")
    }
}

// ---------------------------------------------------------------------------
// Stub wrappers — non-wasm32 targets (compile-time safety)
// ---------------------------------------------------------------------------

/// Stub: always panics. `is_cancelled` is only available in WASM blocks.
#[cfg(not(target_arch = "wasm32"))]
pub fn is_cancelled() -> bool {
    panic!("is_cancelled is only available in WASM blocks")
}

/// Stub: always panics. `log` (host import) is only available in WASM blocks.
#[cfg(not(target_arch = "wasm32"))]
pub fn log(_level: &str, _msg: &str) {
    panic!("log (host import) is only available in WASM blocks")
}

/// Stub: always panics. `call_block` is only available in WASM blocks.
#[cfg(not(target_arch = "wasm32"))]
pub fn call_block(_name: &str, _msg: &Message) -> BlockResult {
    panic!("call_block is only available in WASM blocks")
}
