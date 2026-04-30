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
// CallBlockError — surfaced by the safe `call_block` wrapper
// ---------------------------------------------------------------------------

/// Error returned by [`call_block`] when the target block does not produce
/// a successful response (errored, dropped, or asked the caller to continue).
#[derive(Debug, Clone)]
pub enum CallBlockError {
    /// Target block returned an error terminal.
    Error(WaferError),
    /// Target block dropped the request (no response, no error).
    Drop,
    /// Target block produced a `Continue(Message)` terminal — generally not
    /// useful for synchronous block-to-block calls.
    Continue(Message),
    /// Response could not be parsed (host bug or ABI mismatch).
    Malformed(String),
}

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
    fn __wafer_host_call_block(
        name_ptr: i32,
        name_len: i32,
        msg_ptr: i32,
        msg_len: i32,
        body_ptr: i32,
        body_len: i32,
    ) -> i64;
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

/// Call another block by name, passing a [`Message`] and a body of bytes.
///
/// Mirrors the receiving block's `handle(msg, body)` signature. On success
/// returns the response `(Message, Vec<u8>)`; the `Message` is the response
/// trailer (currently always empty meta — reserved for future use), and the
/// `Vec<u8>` is the response body produced by the target's `OutputStream`.
///
/// Returns [`CallBlockError`] if the target produced an error terminal,
/// dropped the request, asked to continue, or if the host response was
/// malformed.
#[cfg(target_arch = "wasm32")]
pub fn call_block(
    name: &str,
    msg: Message,
    body: &[u8],
) -> Result<(Message, Vec<u8>), CallBlockError> {
    let msg_bytes = serde_json::to_vec(&msg).expect("failed to serialize message");
    let raw = unsafe {
        let packed = __wafer_host_call_block(
            name.as_ptr() as i32,
            name.len() as i32,
            msg_bytes.as_ptr() as i32,
            msg_bytes.len() as i32,
            body.as_ptr() as i32,
            body.len() as i32,
        );
        let (ptr, len) = unpack_ptr_len(packed);
        let slice = std::slice::from_raw_parts(ptr as *const u8, len as usize);
        let bytes = slice.to_vec();
        // Reclaim the allocation made by __wafer_alloc to avoid leaking guest memory.
        let _ = Vec::from_raw_parts(ptr as *mut u8, len as usize, len as usize);
        bytes
    };

    decode_call_block_result(&raw)
}

/// Target-independent decoder: turn a raw `GuestResult` JSON byte slice
/// (the host's response to `__wafer_host_call_block`) into the
/// `Result<(Message, Vec<u8>), CallBlockError>` surfaced by [`call_block`].
///
/// Extracted from [`call_block`] so it can be unit-tested on non-wasm32
/// targets where the underlying FFI is not available.
pub fn decode_call_block_result(raw: &[u8]) -> Result<(Message, Vec<u8>), CallBlockError> {
    let result: GuestResult = serde_json::from_slice(raw)
        .map_err(|e| CallBlockError::Malformed(format!("decoding GuestResult: {e}")))?;

    match result.action.as_str() {
        "Respond" => {
            let response = result.response.ok_or_else(|| {
                CallBlockError::Malformed("Respond action missing response payload".to_string())
            })?;
            // The response trailer message is reserved for future use; today
            // there is no per-call trailer, so synthesize an empty one with
            // any meta that came back on the response.
            let mut trailer = Message::new("response");
            trailer.meta = response.meta;
            Ok((trailer, response.data))
        }
        "Error" => {
            let err = result.error.ok_or_else(|| {
                CallBlockError::Malformed("Error action missing error payload".to_string())
            })?;
            Err(CallBlockError::Error(err))
        }
        "Drop" => Err(CallBlockError::Drop),
        "Continue" => {
            let cont = result.message.ok_or_else(|| {
                CallBlockError::Malformed("Continue action missing message payload".to_string())
            })?;
            Err(CallBlockError::Continue(cont))
        }
        other => Err(CallBlockError::Malformed(format!(
            "unknown action '{other}'"
        ))),
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
pub fn call_block(
    _name: &str,
    _msg: Message,
    _body: &[u8],
) -> Result<(Message, Vec<u8>), CallBlockError> {
    panic!("call_block is only available in WASM blocks")
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

    #[test]
    fn decode_respond_returns_body() {
        // Simulate a host returning a Respond GuestResult with a non-empty
        // response body — the decoder must hand back those bytes.
        let payload = serde_json::json!({
            "action": "Respond",
            "response": { "data": [104, 105], "meta": [] },
            "error": null,
            "message": null,
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let (msg, body) = decode_call_block_result(&raw).expect("Respond should decode");
        assert_eq!(body, b"hi");
        assert!(msg.meta.is_empty());
    }

    #[test]
    fn decode_respond_preserves_response_meta() {
        let payload = serde_json::json!({
            "action": "Respond",
            "response": {
                "data": [],
                "meta": [{ "key": "x-trailer", "value": "v" }],
            },
            "error": null,
            "message": null,
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let (msg, body) = decode_call_block_result(&raw).unwrap();
        assert!(body.is_empty());
        assert_eq!(msg.meta.len(), 1);
        assert_eq!(msg.meta[0].key, "x-trailer");
        assert_eq!(msg.meta[0].value, "v");
    }

    #[test]
    fn decode_error_maps_to_callblockerror_error() {
        let payload = serde_json::json!({
            "action": "Error",
            "response": null,
            "error": { "code": "Internal", "message": "boom", "meta": [] },
            "message": null,
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        match decode_call_block_result(&raw) {
            Err(CallBlockError::Error(e)) => assert_eq!(e.message, "boom"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn decode_drop_maps_to_callblockerror_drop() {
        let payload = serde_json::json!({
            "action": "Drop",
            "response": null,
            "error": null,
            "message": null,
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        match decode_call_block_result(&raw) {
            Err(CallBlockError::Drop) => {}
            other => panic!("expected Drop, got {other:?}"),
        }
    }

    #[test]
    fn decode_malformed_json_is_malformed_error() {
        let raw = b"not json";
        match decode_call_block_result(raw) {
            Err(CallBlockError::Malformed(_)) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }
}
