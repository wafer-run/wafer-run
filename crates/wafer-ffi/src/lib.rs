//! wafer-ffi — C shared library exposing the WAFER runtime.
//!
//! Design:
//! - Rust owns all memory; callers hold an opaque `*mut WaferRuntime` pointer.
//! - All complex data crosses the FFI boundary as JSON C strings.
//! - Caller must free returned strings via `wafer_free_string()`.
//! - Functions that can fail return NULL on success, or a JSON error string.
//! - Panics are caught at every FFI boundary.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};

use wafer_run::{FlowDef, Message, Result_, Wafer, WASMBlock};

/// Opaque handle wrapping the Rust runtime.
pub struct WaferRuntime {
    inner: Wafer,
    /// Tokio runtime for bridging async calls at the FFI boundary.
    rt: tokio::runtime::Runtime,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a Rust string into a heap-allocated C string (caller must free via
/// `wafer_free_string`).  Returns a null pointer if the string contains
/// interior NUL bytes (should never happen with JSON).
fn to_c_string(s: &str) -> *mut c_char {
    match CString::new(s) {
        Ok(cs) => cs.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Build a JSON error string: `{"error":"<msg>"}`.
fn error_json(msg: &str) -> *mut c_char {
    let escaped = msg.replace('\\', "\\\\").replace('"', "\\\"");
    let json = format!(r#"{{"error":"{}"}}"#, escaped);
    to_c_string(&json)
}

/// Safely dereference a `*mut WaferRuntime`.
/// Returns `None` (and a null-safe no-op) when the pointer is null.
unsafe fn deref_mut<'a>(ptr: *mut WaferRuntime) -> Option<&'a mut WaferRuntime> {
    if ptr.is_null() {
        None
    } else {
        Some(&mut *ptr)
    }
}

unsafe fn deref_ref<'a>(ptr: *mut WaferRuntime) -> Option<&'a WaferRuntime> {
    if ptr.is_null() {
        None
    } else {
        Some(&*ptr)
    }
}

/// Read a `*const c_char` into a `&str`. Returns `None` on null or invalid UTF-8.
unsafe fn c_str_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        None
    } else {
        CStr::from_ptr(ptr).to_str().ok()
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Create a new WAFER runtime instance.
#[no_mangle]
pub extern "C" fn wafer_new() -> *mut WaferRuntime {
    let result = std::panic::catch_unwind(|| {
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        let wr = WaferRuntime {
            inner: Wafer::new(),
            rt,
        };
        Box::into_raw(Box::new(wr))
    });
    result.unwrap_or(std::ptr::null_mut())
}

/// Free a WAFER runtime instance.
#[no_mangle]
pub unsafe extern "C" fn wafer_free(w: *mut WaferRuntime) {
    if !w.is_null() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(Box::from_raw(w));
        }));
    }
}

/// Resolve all block references in registered flows.
/// Returns NULL on success, or a JSON error string on failure.
#[no_mangle]
pub unsafe extern "C" fn wafer_resolve(w: *mut WaferRuntime) -> *mut c_char {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rt = match deref_mut(w) {
            Some(r) => r,
            None => return error_json("null runtime pointer"),
        };
        match rt.rt.block_on(rt.inner.resolve()) {
            Ok(()) => std::ptr::null_mut(),
            Err(e) => error_json(&e),
        }
    }));
    result.unwrap_or_else(|_| error_json("panic in wafer_resolve"))
}

/// Start the runtime (without spawning block listeners).
/// Returns NULL on success, or a JSON error string on failure.
#[no_mangle]
pub unsafe extern "C" fn wafer_start(w: *mut WaferRuntime) -> *mut c_char {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rt = match deref_mut(w) {
            Some(r) => r,
            None => return error_json("null runtime pointer"),
        };
        match rt.rt.block_on(rt.inner.start_without_bind()) {
            Ok(()) => std::ptr::null_mut(),
            Err(e) => error_json(&e),
        }
    }));
    result.unwrap_or_else(|_| error_json("panic in wafer_start"))
}

/// Stop the runtime.
#[no_mangle]
pub unsafe extern "C" fn wafer_stop(w: *mut WaferRuntime) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(rt) = deref_mut(w) {
            rt.rt.block_on(rt.inner.stop());
        }
    }));
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register a block or flow definition from a file path.
/// If `path` ends with `.wasm`, registers a WASM block with the given name.
/// Otherwise, reads the file as a JSON flow definition.
/// Returns NULL on success, or a JSON error string on failure.
#[no_mangle]
pub unsafe extern "C" fn wafer_register(
    w: *mut WaferRuntime,
    name: *const c_char,
    path: *const c_char,
) -> *mut c_char {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rt = match deref_mut(w) {
            Some(r) => r,
            None => return error_json("null runtime pointer"),
        };
        let name_str = match c_str_to_str(name) {
            Some(s) => s,
            None => return error_json("invalid name"),
        };
        let path_str = match c_str_to_str(path) {
            Some(s) => s,
            None => return error_json("invalid path"),
        };

        if path_str.ends_with(".wasm") {
            match WASMBlock::load(path_str) {
                Ok(block) => {
                    rt.inner.register_block(name_str, std::sync::Arc::new(block));
                    std::ptr::null_mut()
                }
                Err(e) => error_json(&e),
            }
        } else {
            match std::fs::read_to_string(path_str) {
                Ok(json) => {
                    let def: FlowDef = match serde_json::from_str(&json) {
                        Ok(d) => d,
                        Err(e) => return error_json(&format!("invalid FlowDef JSON: {}", e)),
                    };
                    rt.inner.add_flow_def(&def);
                    std::ptr::null_mut()
                }
                Err(e) => error_json(&format!("failed to read file: {}", e)),
            }
        }
    }));
    result.unwrap_or_else(|_| error_json("panic in wafer_register"))
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Run a flow with the given message.
/// Returns a JSON result string (always non-NULL).
#[no_mangle]
pub unsafe extern "C" fn wafer_run(
    w: *mut WaferRuntime,
    flow_id: *const c_char,
    message_json: *const c_char,
) -> *mut c_char {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rt = match deref_mut(w) {
            Some(r) => r,
            None => {
                return to_c_string(
                    &serde_json::to_string(&Result_::error(wafer_run::WaferError::new(
                        "ffi_error",
                        "null runtime pointer",
                    )))
                    .unwrap_or_else(|_| r#"{"action":"error","error":{"code":"ffi_error","message":"null runtime pointer"}}"#.to_string()),
                );
            }
        };
        let fid = match c_str_to_str(flow_id) {
            Some(s) => s,
            None => {
                return to_c_string(
                    &serde_json::to_string(&Result_::error(wafer_run::WaferError::new(
                        "ffi_error",
                        "invalid flow_id",
                    )))
                    .unwrap_or_else(|_| r#"{"action":"error","error":{"code":"ffi_error","message":"invalid flow_id"}}"#.to_string()),
                );
            }
        };
        let msg_str = match c_str_to_str(message_json) {
            Some(s) => s,
            None => {
                return to_c_string(
                    &serde_json::to_string(&Result_::error(wafer_run::WaferError::new(
                        "ffi_error",
                        "invalid message_json",
                    )))
                    .unwrap_or_else(|_| r#"{"action":"error","error":{"code":"ffi_error","message":"invalid message_json"}}"#.to_string()),
                );
            }
        };

        let mut msg: Message = match serde_json::from_str(msg_str) {
            Ok(m) => m,
            Err(e) => {
                let err_result = Result_::error(wafer_run::WaferError::new(
                    "ffi_error",
                    format!("invalid Message JSON: {}", e),
                ));
                return to_c_string(
                    &serde_json::to_string(&err_result).unwrap_or_else(|_| {
                        r#"{"action":"error","error":{"code":"ffi_error","message":"json error"}}"#
                            .to_string()
                    }),
                );
            }
        };

        let result = rt.rt.block_on(rt.inner.execute(fid, &mut msg));

        to_c_string(&serde_json::to_string(&result).unwrap_or_else(|_| {
            r#"{"action":"error","error":{"code":"ffi_error","message":"failed to serialize result"}}"#
                .to_string()
        }))
    }));
    result.unwrap_or_else(|_| {
        to_c_string(
            r#"{"action":"error","error":{"code":"ffi_error","message":"panic in wafer_run"}}"#,
        )
    })
}

// ---------------------------------------------------------------------------
// Introspection
// ---------------------------------------------------------------------------

/// Get info about all registered flows as a JSON array.
#[no_mangle]
pub unsafe extern "C" fn wafer_flows_info(w: *mut WaferRuntime) -> *mut c_char {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rt = match deref_ref(w) {
            Some(r) => r,
            None => return to_c_string("[]"),
        };
        let info = rt.inner.flows_info();
        to_c_string(&serde_json::to_string(&info).unwrap_or_else(|_| "[]".to_string()))
    }));
    result.unwrap_or_else(|_| to_c_string("[]"))
}

/// Check whether a block type is registered.
/// Returns 1 if registered, 0 if not.
#[no_mangle]
pub unsafe extern "C" fn wafer_has_block(
    w: *mut WaferRuntime,
    type_name: *const c_char,
) -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rt = match deref_ref(w) {
            Some(r) => r,
            None => return 0,
        };
        let name = match c_str_to_str(type_name) {
            Some(s) => s,
            None => return 0,
        };
        if rt.inner.has_block(name) {
            1
        } else {
            0
        }
    }));
    result.unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// Free a string previously returned by any wafer_* function.
#[no_mangle]
pub unsafe extern "C" fn wafer_free_string(s: *mut c_char) {
    if !s.is_null() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(CString::from_raw(s));
        }));
    }
}
