//! wafer-ffi — C shared library exposing the WAFER runtime.
//!
//! Design:
//! - Rust owns all memory; callers hold an opaque `*mut WaferRuntime` pointer.
//! - All complex data crosses the FFI boundary as JSON C strings.
//! - Operations that perform async work (`wafer_resolve`, `wafer_start`,
//!   `wafer_stop`, `wafer_run`) are callback-based: they return immediately
//!   after spawning the work onto an internal tokio runtime, and invoke the
//!   supplied `wafer_done_cb` when the work completes. The result pointer
//!   passed to the callback is owned by Rust and freed after the callback
//!   returns — callers must copy any data they need before returning.
//! - Synchronous ops (`wafer_new`, `wafer_free`, `wafer_register`,
//!   `wafer_flows_info`, `wafer_has_block`) return immediately with a result;
//!   strings they return must be freed via `wafer_free_string`.
//! - Functions that can fail signal failure via the callback's non-NULL
//!   result (lifecycle ops) or a JSON error in the returned string
//!   (synchronous ops).
//! - Panics are caught at every FFI boundary.
#![allow(clippy::missing_safety_doc)]

use std::{
    ffi::{c_void, CStr, CString},
    os::raw::{c_char, c_int},
    sync::Arc,
};

use tokio::sync::RwLock;
use wafer_run::{Message, StaticConfigSource, Wafer, WasmiBlock};

/// Callback invoked when an async FFI op completes.
///
/// - For lifecycle ops (`wafer_resolve`/`wafer_start`/`wafer_stop`): `result`
///   is NULL on success, or a JSON error string on failure.
/// - For `wafer_run`: `result` is always non-NULL — a JSON result string of
///   the form `{"action":"respond|drop|error|continue", ...}`.
///
/// The `result` pointer is owned by the FFI layer and freed after the
/// callback returns; callers must copy what they need before returning.
/// `user_data` is opaque to the FFI layer and passed through unchanged.
///
/// The callback may be invoked from any thread owned by the FFI's internal
/// tokio runtime; consumers are responsible for thread-safety inside the
/// callback.
pub type WaferDoneCb = unsafe extern "C" fn(result: *const c_char, user_data: *mut c_void);

/// Opaque handle wrapping the Rust runtime.
pub struct WaferRuntime {
    inner: Arc<RwLock<Wafer>>,
    /// Tokio runtime that drives spawned async work. Not used to block_on
    /// anything; futures are `spawn`'d and signal completion via the caller's
    /// `WaferDoneCb`.
    rt: tokio::runtime::Runtime,
}

/// `*mut c_void` is not `Send` by default. We need to move the opaque
/// `user_data` pointer into spawned futures, so wrap it in a newtype with a
/// hand-rolled `Send` impl. The pointer is opaque to Rust — its lifetime and
/// thread-safety are the C caller's responsibility.
struct UserData(*mut c_void);
unsafe impl Send for UserData {}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a Rust string into a heap-allocated C string (caller must free via
/// `wafer_free_string`). Returns a null pointer if the string contains
/// interior NUL bytes (should never happen with JSON).
fn to_c_string(s: &str) -> *mut c_char {
    match CString::new(s) {
        Ok(cs) => cs.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Build a JSON error CString: `{"error":"<msg>"}`.
///
/// Uses `serde_json::to_string` for escaping so values containing
/// backslashes, embedded quotes, control characters, or non-ASCII
/// bytes are emitted as valid JSON. The previous hand-rolled escape
/// only handled `\\` and `"` and would emit invalid JSON for any
/// input containing a literal `\n` or `\t`.
fn error_cstring(msg: &str) -> CString {
    let json = serde_json::to_string(&serde_json::json!({ "error": msg }))
        .unwrap_or_else(|_| String::from(r#"{"error":"unprintable"}"#));
    CString::new(json).unwrap_or_else(|_| CString::new(r#"{"error":"unprintable"}"#).unwrap())
}

/// Build a JSON error string allocated for caller-free: `{"error":"<msg>"}`.
fn error_json(msg: &str) -> *mut c_char {
    let json = serde_json::to_string(&serde_json::json!({ "error": msg }))
        .unwrap_or_else(|_| String::from(r#"{"error":"unprintable"}"#));
    to_c_string(&json)
}

/// Build a JSON result string from a collected OutputStream.
async fn output_to_json(output: wafer_run::OutputStream) -> String {
    use wafer_block::streams::output::TerminalNotResponse;
    match output.collect_buffered().await {
        Ok(buf) => {
            let body_str = String::from_utf8(buf.body).unwrap_or_default();
            let meta_obj: serde_json::Value = buf
                .meta
                .iter()
                .map(|e| (e.key.clone(), serde_json::Value::String(e.value.clone())))
                .collect::<serde_json::Map<_, _>>()
                .into();
            serde_json::json!({
                "action": "respond",
                "body": body_str,
                "meta": meta_obj,
            })
            .to_string()
        }
        Err(TerminalNotResponse::Error(err)) => serde_json::json!({
            "action": "error",
            "error": {
                "code": format!("{:?}", err.code),
                "message": err.message,
            }
        })
        .to_string(),
        Err(TerminalNotResponse::Drop) => serde_json::json!({ "action": "drop" }).to_string(),
        Err(TerminalNotResponse::Continue(msg)) => serde_json::json!({
            "action": "continue",
            "message": serde_json::to_value(&msg).unwrap_or_default(),
        })
        .to_string(),
        Err(TerminalNotResponse::Malformed) => serde_json::json!({
            "action": "error",
            "error": { "code": "Internal", "message": "stream ended without terminal event" }
        })
        .to_string(),
    }
}

/// Safely dereference a `*mut WaferRuntime` to `&WaferRuntime`.
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

/// Invoke a `WaferDoneCb` with a possibly-null result. The CString backing the
/// pointer is dropped after the callback returns. Takes `UserData` (rather
/// than the raw `*mut c_void`) so async blocks can pass the wrapper through
/// without ever exposing the non-`Send` pointer as a local.
unsafe fn invoke_done(cb: WaferDoneCb, result: Option<CString>, ud: UserData) {
    let ptr = result
        .as_ref()
        .map(|c| c.as_ptr())
        .unwrap_or(std::ptr::null());
    cb(ptr, ud.0);
    // `result` dropped here — after the callback has consumed the pointer.
    drop(result);
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Install a once-only panic hook that logs panics via tracing before
/// the surrounding `catch_unwind` rolls them up into a JSON error
/// string. Without this, the caller only sees the panic message — the
/// originating file:line:backtrace is lost. Idempotent; safe to call
/// from any FFI entry point.
fn install_panic_logger_once() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // tracing might not be initialized in every embedding; the
            // default hook's stderr output is the failsafe.
            tracing::error!(panic = %info, "wafer-ffi: panic crossing FFI boundary");
            default(info);
        }));
    });
}

/// Create a new WAFER runtime instance.
#[no_mangle]
pub extern "C" fn wafer_new() -> *mut WaferRuntime {
    install_panic_logger_once();
    let result = std::panic::catch_unwind(|| {
        let rt = tokio::runtime::Runtime::new().ok()?;
        let inner = Wafer::new(Arc::new(StaticConfigSource::default())).ok()?;
        let wr = WaferRuntime {
            inner: Arc::new(RwLock::new(inner)),
            rt,
        };
        Some(Box::into_raw(Box::new(wr)))
    });
    result.ok().flatten().unwrap_or(std::ptr::null_mut())
}

/// Free a WAFER runtime instance.
///
/// The caller must first call `wafer_stop` and wait for its callback to fire
/// before calling `wafer_free`; otherwise block `lifecycle(Stop)` handlers
/// will not run. After `wafer_free`, the tokio runtime is dropped, which
/// waits for any in-flight spawned tasks to complete.
#[no_mangle]
pub unsafe extern "C" fn wafer_free(w: *mut WaferRuntime) {
    if !w.is_null() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(Box::from_raw(w));
        }));
    }
}

/// Resolve all block references in registered flows (async).
///
/// Returns immediately; invokes `cb` when resolution (`seal()`) completes.
/// On success the callback's `result` is NULL; on failure it is a JSON
/// error string.
///
/// `seal()` performs composite-config expansion, `uses` gathering,
/// capability resolution, remote-block download, and startup-snapshot
/// finalization, but does **not** dispatch `lifecycle(Init)` eagerly.
/// Per-block `Init` runs lazily on first dispatch per worker isolate —
/// call `wafer_run_block` (or `wafer_run_flow`) to trigger init on a
/// specific block. To validate config without dispatching, call
/// `wafer_validate_all_block_configs` (FFI bindings TBD; see
/// `Wafer::validate_all_block_configs`).
#[no_mangle]
pub unsafe extern "C" fn wafer_resolve(
    w: *mut WaferRuntime,
    cb: WaferDoneCb,
    user_data: *mut c_void,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = deref_ref(w) else {
            invoke_done(
                cb,
                Some(error_cstring("null runtime pointer")),
                UserData(user_data),
            );
            return;
        };
        let inner = runtime.inner.clone();
        let ud = UserData(user_data);
        runtime.rt.spawn(async move {
            let result = inner.write().await.seal().await;
            let err = match result {
                Ok(()) => None,
                Err(e) => Some(error_cstring(&e.to_string())),
            };
            invoke_done(cb, err, ud);
        });
    }));
    if result.is_err() {
        invoke_done(
            cb,
            Some(error_cstring("panic in wafer_resolve")),
            UserData(user_data),
        );
    }
}

/// Start the runtime without spawning block listeners (async).
///
/// Returns immediately; invokes `cb` when start (`seal()`) completes. On
/// success the callback's `result` is NULL; on failure it is a JSON
/// error string.
///
/// `seal()` performs composite-config expansion, `uses` gathering,
/// capability resolution, remote-block download, and startup-snapshot
/// finalization, but does **not** dispatch `lifecycle(Init)` eagerly.
/// Per-block `Init` runs lazily on first dispatch per worker isolate —
/// call `wafer_run_block` (or `wafer_run_flow`) to trigger init on a
/// specific block. To validate config without dispatching, call
/// `wafer_validate_all_block_configs` (FFI bindings TBD; see
/// `Wafer::validate_all_block_configs`).
#[no_mangle]
pub unsafe extern "C" fn wafer_start(
    w: *mut WaferRuntime,
    cb: WaferDoneCb,
    user_data: *mut c_void,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = deref_ref(w) else {
            invoke_done(
                cb,
                Some(error_cstring("null runtime pointer")),
                UserData(user_data),
            );
            return;
        };
        let inner = runtime.inner.clone();
        let ud = UserData(user_data);
        runtime.rt.spawn(async move {
            let result = inner.write().await.seal().await;
            let err = match result {
                Ok(()) => None,
                Err(e) => Some(error_cstring(&e.to_string())),
            };
            invoke_done(cb, err, ud);
        });
    }));
    if result.is_err() {
        invoke_done(
            cb,
            Some(error_cstring("panic in wafer_start")),
            UserData(user_data),
        );
    }
}

/// Stop the runtime and shut down all block instances (async).
///
/// Returns immediately; invokes `cb` (with NULL result) when shutdown
/// completes. Must be called before `wafer_free` for block `lifecycle(Stop)`
/// handlers to run.
#[no_mangle]
pub unsafe extern "C" fn wafer_stop(w: *mut WaferRuntime, cb: WaferDoneCb, user_data: *mut c_void) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = deref_ref(w) else {
            invoke_done(
                cb,
                Some(error_cstring("null runtime pointer")),
                UserData(user_data),
            );
            return;
        };
        let inner = runtime.inner.clone();
        let ud = UserData(user_data);
        runtime.rt.spawn(async move {
            inner.write().await.stop().await;
            invoke_done(cb, None, ud);
        });
    }));
    if result.is_err() {
        invoke_done(
            cb,
            Some(error_cstring("panic in wafer_stop")),
            UserData(user_data),
        );
    }
}

// ---------------------------------------------------------------------------
// Registration (synchronous — no async work)
// ---------------------------------------------------------------------------

/// Register a block or flow definition from a file path.
///
/// If `path` ends with `.wasm`, registers a WASM block with the given name.
/// Otherwise, reads the file as a JSON flow definition. Returns NULL on
/// success, or a JSON error string on failure. Caller must free the returned
/// string with `wafer_free_string`.
#[no_mangle]
pub unsafe extern "C" fn wafer_register(
    w: *mut WaferRuntime,
    name: *const c_char,
    path: *const c_char,
) -> *mut c_char {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = deref_ref(w) else {
            return error_json("null runtime pointer");
        };
        let Some(name_str) = c_str_to_str(name) else {
            return error_json("invalid name");
        };
        let Some(path_str) = c_str_to_str(path) else {
            return error_json("invalid path");
        };

        // register_block / add_flow_json are sync — block_on isn't needed.
        // We acquire the write lock blockingly because we're guaranteed to be
        // on the C caller's thread (no tokio context expected).
        let mut inner = runtime.inner.blocking_write();
        if path_str.ends_with(".wasm") {
            match WasmiBlock::load(path_str) {
                Ok(block) => match inner.register_block(name_str, Arc::new(block)) {
                    Ok(()) => std::ptr::null_mut(),
                    Err(e) => error_json(&e.to_string()),
                },
                Err(e) => error_json(&e.to_string()),
            }
        } else {
            match std::fs::read_to_string(path_str) {
                Ok(json) => match inner.add_flow_json(&json) {
                    Ok(()) => std::ptr::null_mut(),
                    Err(e) => error_json(&format!("invalid WaferFlow JSON: {e}")),
                },
                Err(e) => error_json(&format!("failed to read file: {e}")),
            }
        }
    }));
    result.unwrap_or_else(|_| error_json("panic in wafer_register"))
}

// ---------------------------------------------------------------------------
// Execution (async)
// ---------------------------------------------------------------------------

/// Run a flow with the given message (body-less). Async.
///
/// Returns immediately; invokes `cb` with the JSON result string when the
/// flow finishes. `cb`'s `result` is always non-NULL — a JSON object of the
/// form `{"action":"respond|drop|error|continue", ...}`.
#[no_mangle]
pub unsafe extern "C" fn wafer_run(
    w: *mut WaferRuntime,
    flow_id: *const c_char,
    message_json: *const c_char,
    cb: WaferDoneCb,
    user_data: *mut c_void,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let internal_err = |code: &str, msg: &str| {
            CString::new(format!(
                r#"{{"action":"error","error":{{"code":"{}","message":"{}"}}}}"#,
                code,
                msg.replace('"', "\\\"")
            ))
            .unwrap_or_else(|_| CString::new("{}").unwrap())
        };

        let Some(runtime) = deref_ref(w) else {
            invoke_done(
                cb,
                Some(internal_err("Internal", "null runtime pointer")),
                UserData(user_data),
            );
            return;
        };
        let Some(fid) = c_str_to_str(flow_id) else {
            invoke_done(
                cb,
                Some(internal_err("Internal", "invalid flow_id")),
                UserData(user_data),
            );
            return;
        };
        let Some(msg_str) = c_str_to_str(message_json) else {
            invoke_done(
                cb,
                Some(internal_err("Internal", "invalid message_json")),
                UserData(user_data),
            );
            return;
        };

        let msg: Message = match serde_json::from_str(msg_str) {
            Ok(m) => m,
            Err(e) => {
                invoke_done(
                    cb,
                    Some(internal_err(
                        "InvalidArgument",
                        &format!("invalid Message JSON: {e}"),
                    )),
                    UserData(user_data),
                );
                return;
            }
        };

        let inner = runtime.inner.clone();
        let flow_id = fid.to_owned();
        let ud = UserData(user_data);
        runtime.rt.spawn(async move {
            let input = wafer_run::InputStream::empty();
            let output = inner.read().await.run(&flow_id, msg, input).await;
            let json = output_to_json(output).await;
            let cs = CString::new(json).unwrap_or_else(|_| CString::new("{}").unwrap());
            invoke_done(cb, Some(cs), ud);
        });
    }));
    if result.is_err() {
        invoke_done(
            cb,
            Some(error_cstring("panic in wafer_run")),
            UserData(user_data),
        );
    }
}

// ---------------------------------------------------------------------------
// Introspection (synchronous — read-only, no async work)
// ---------------------------------------------------------------------------

/// Get info about all registered flows as a JSON array.
#[no_mangle]
pub unsafe extern "C" fn wafer_flows_info(w: *mut WaferRuntime) -> *mut c_char {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = deref_ref(w) else {
            return to_c_string("[]");
        };
        let info = runtime.inner.blocking_read().flows_info();
        to_c_string(&serde_json::to_string(&info).unwrap_or_else(|_| "[]".to_string()))
    }));
    result.unwrap_or_else(|_| to_c_string("[]"))
}

/// Check whether a block type is registered. Returns 1 if registered, 0 if not.
#[no_mangle]
pub unsafe extern "C" fn wafer_has_block(w: *mut WaferRuntime, type_name: *const c_char) -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = deref_ref(w) else {
            return 0;
        };
        let Some(name) = c_str_to_str(type_name) else {
            return 0;
        };
        if runtime.inner.blocking_read().has_block(name) {
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

/// Free a string previously returned by a synchronous `wafer_*` function.
/// Async callbacks receive Rust-owned strings that are freed automatically
/// when the callback returns — do not pass them to this function.
#[no_mangle]
pub unsafe extern "C" fn wafer_free_string(s: *mut c_char) {
    if !s.is_null() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(CString::from_raw(s));
        }));
    }
}
