//! Core ABI for WASM blocks running on wasmi.
//!
//! Provides:
//! - Pack/unpack helpers for the `i64` `(ptr << 32 | len)` pointer convention.
//! - The `__wafer_alloc` export so the host can allocate guest memory.
//! - Safe wrappers around the host imports (`call_block`, `log`, `is_cancelled`).
//! - [`GuestResult`] / [`GuestResponse`]: the JSON-serializable result type that
//!   `__wafer_handle` returns to the host. The `#[wafer_block]` macro generates
//!   the ABI glue; block authors use these types directly in their `handle` impl.
//!
//! The `extern "C"` FFI declarations and the `__wafer_alloc` export are only
//! meaningful on `wasm32` targets. On all other targets the public wrappers
//! compile to stub functions that panic, ensuring block code that calls them
//! fails loudly rather than silently doing nothing.

use wafer_block::{Message, MetaEntry, WaferError};

// ---------------------------------------------------------------------------
// Guest result types — returned from __wafer_handle / __wafer_lifecycle
// ---------------------------------------------------------------------------

/// The result returned by a WASM block's `handle` function.
///
/// This is serialized as JSON and sent back to the host runtime. The host
/// maps it to an `OutputStream` using the `LegacyResult` bridge layer.
///
/// Use [`GuestResult::respond`], [`GuestResult::error`], or
/// [`GuestResult::drop_request`] to construct values.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GuestResult {
    pub action: String,
    pub response: Option<GuestResponse>,
    pub error: Option<WaferError>,
    pub message: Option<Message>,
}

/// The response body returned by a successful WASM block invocation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GuestResponse {
    pub data: Vec<u8>,
    pub meta: Vec<MetaEntry>,
}

impl GuestResult {
    /// Respond with a body (and optional trailing meta).
    pub fn respond(data: Vec<u8>) -> Self {
        Self {
            action: "Respond".to_string(),
            response: Some(GuestResponse { data, meta: vec![] }),
            error: None,
            message: None,
        }
    }

    /// Respond with body and meta entries.
    pub fn respond_with_meta(data: Vec<u8>, meta: Vec<MetaEntry>) -> Self {
        Self {
            action: "Respond".to_string(),
            response: Some(GuestResponse { data, meta }),
            error: None,
            message: None,
        }
    }

    /// Return an error to the caller.
    pub fn error(err: WaferError) -> Self {
        Self {
            action: "Error".to_string(),
            response: None,
            error: Some(err),
            message: None,
        }
    }

    /// Drop the request (no response, no error).
    pub fn drop_request() -> Self {
        Self {
            action: "Drop".to_string(),
            response: None,
            error: None,
            message: None,
        }
    }
}

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

/// Call another block by name, passing a [`Message`].
///
/// Returns the raw JSON response from the host as a `serde_json::Value`.
/// The response object contains an `"action"` field and optional `"response"`,
/// `"error"`, and `"message"` fields.
#[cfg(target_arch = "wasm32")]
pub fn call_block(name: &str, msg: &Message) -> serde_json::Value {
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
        let result: serde_json::Value =
            serde_json::from_slice(bytes).expect("failed to deserialize call_block result");
        // Reclaim the allocation made by __wafer_alloc to avoid leaking guest memory.
        let _ = Vec::from_raw_parts(ptr as *mut u8, len as usize, len as usize);
        result
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
pub fn call_block(_name: &str, _msg: &Message) -> serde_json::Value {
    panic!("call_block is only available in WASM blocks")
}
