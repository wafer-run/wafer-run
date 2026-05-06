//! Core ABI for WASM blocks running on wasmi.
//!
//! Provides:
//! - Pack/unpack helpers for the `i64` `(ptr << 32 | len)` pointer convention.
//! - The `__wafer_alloc` export so the host can allocate guest memory.
//! - Safe wrappers around the host imports (`log`, `is_cancelled`).
//! - [`GuestResult`] / [`GuestResponse`]: the JSON-serializable result type that
//!   `__wafer_handle` returns to the host. The `#[wafer_block]` macro generates
//!   the ABI glue; block authors use these types directly in their `handle` impl.
//!
//! The `extern "C"` FFI declarations and the `__wafer_alloc` export are only
//! meaningful on `wasm32` targets. On all other targets the public wrappers
//! compile to stub functions that panic, ensuring block code that calls them
//! fails loudly rather than silently doing nothing.

use wafer_block::{MetaEntry, WaferError};

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
    pub message: Option<wafer_block::Message>,
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

    /// Begin a streaming call. Returns a positive stream handle (i64), or a
    /// negative i64 packing an ErrorCode in the low 32 bits.
    pub(crate) fn __wafer_host_stream_init(
        name_ptr: i32,
        name_len: i32,
        msg_ptr: i32,
        msg_len: i32,
    ) -> i64;

    /// Append a chunk to the request body. Returns 0 on success, negative ErrorCode otherwise.
    pub(crate) fn __wafer_host_stream_write_chunk(handle: i64, body_ptr: i32, body_len: i32)
        -> i32;

    /// Close the request side; transitions handle to ReadingResponse.
    /// Returns 0 on success, negative ErrorCode otherwise.
    pub(crate) fn __wafer_host_stream_finish(handle: i64) -> i32;

    /// Pull the next response chunk. Returns:
    ///   - positive packed (ptr, len): chunk available; guest owns the allocation
    ///   - 0: end of stream
    ///   - negative: error sentinel; details retrievable via take_error
    pub(crate) fn __wafer_host_stream_read_chunk(handle: i64) -> i64;

    /// Retrieve the most recent error for a stream handle.
    /// Returns packed (ptr, len) of an rmp-serde-encoded WaferError.
    pub(crate) fn __wafer_host_stream_take_error(handle: i64) -> i64;

    /// Free the handle and any host-side state. Idempotent; safe to call
    /// from Drop on the SDK wrappers.
    pub(crate) fn __wafer_host_stream_close(handle: i64);

    /// Attach an rmp-encoded (id, Attachment) tuple to an in-flight stream.
    /// Returns 0 on success, negative ErrorCode otherwise.
    pub(crate) fn __wafer_host_stream_attach(
        handle: i64,
        payload_ptr: i32,
        payload_len: i32,
    ) -> i32;

    /// Look up an inbound attachment by id. Returns negative ErrorCode (NotFound)
    /// if absent, else positive packed (ptr, len) of an rmp-encoded Attachment.
    pub(crate) fn __wafer_host_lookup_attachment(id_ptr: i32, id_len: i32) -> i64;
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

// ---------------------------------------------------------------------------
// Tests — target-independent helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrip() {
        for (ptr, len) in [(0u32, 0u32), (1, 2), (0xDEAD_BEEF, 0x1234)] {
            let packed = pack_ptr_len(ptr, len);
            let (got_ptr, got_len) = unpack_ptr_len(packed);
            assert_eq!(got_ptr, ptr);
            assert_eq!(got_len, len);
        }
    }
}
