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
//!   returns — callers must copy any data they need before returning. The
//!   callback is required: each returns [`WAFER_ACCEPTED`], or
//!   [`WAFER_REFUSED_NULL_CALLBACK`] without doing anything when it is NULL.
//! - Synchronous ops (`wafer_new`, `wafer_free`, `wafer_register`,
//!   `wafer_register_block`, `wafer_flows_info`, `wafer_has_block`) return
//!   immediately with a result; strings they return must be freed via
//!   `wafer_free_string`.
//! - Functions that can fail signal failure via the callback's non-NULL
//!   result (lifecycle ops) or a JSON error in the returned string
//!   (synchronous ops).
//! - Panics are caught at every FFI boundary.
#![warn(missing_docs)]
#![allow(clippy::missing_safety_doc)]

use std::{
    ffi::{c_void, CStr, CString},
    os::raw::{c_char, c_int},
    sync::Arc,
};

use tokio::sync::RwLock;
use wafer_run::{Message, SealState, StaticConfigSource, Wafer};

/// Callback invoked when an async FFI op completes.
///
/// - For lifecycle ops (`wafer_resolve`/`wafer_start`/`wafer_stop`): `result`
///   is NULL on success, or a JSON error string on failure.
/// - For `wafer_run`: `result` is always non-NULL — a JSON result string of
///   the form `{"action":"respond|drop|error|continue|halt", ...}`. Its
///   `meta` object holds only response entries: `resp.status`,
///   `resp.content_type`, `resp.header.{name}` and `resp.set_cookie.{id}`,
///   whose value is one whole `Set-Cookie` directive (read nothing from
///   `{id}`); request state never crosses this boundary. The wire format
///   (including the `body` vs `body_base64` rules for `respond` and `halt`)
///   is documented on [`wafer_run::embed::output_to_json`], which produces
///   it.
///
/// The `result` pointer is owned by the FFI layer and freed after the
/// callback returns; callers must copy what they need before returning.
/// `user_data` is opaque to the FFI layer and passed through unchanged.
///
/// The callback may be invoked from any thread owned by the FFI's internal
/// tokio runtime; consumers are responsible for thread-safety inside the
/// callback.
///
/// The async entry points take it as `Option<WaferDoneCb>`, which has the
/// same ABI as the C function pointer with NULL as `None`.
pub type WaferDoneCb = unsafe extern "C" fn(result: *const c_char, user_data: *mut c_void);

/// Returned by an async entry point that took the work: its callback will be
/// invoked when the work completes.
pub const WAFER_ACCEPTED: c_int = 0;

/// Returned by an async entry point whose callback is NULL. Nothing was done
/// and nothing will call back: every async op's completion is load-bearing
/// (a `wafer_stop` must finish before `wafer_free`, a `wafer_run`'s result
/// is its output), so there is no fire-and-forget form.
pub const WAFER_REFUSED_NULL_CALLBACK: c_int = -1;

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
    CString::new(s).map_or(std::ptr::null_mut(), CString::into_raw)
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
#[expect(
    clippy::needless_pass_by_value,
    reason = "UserData owns the non-Send C pointer and is consumed here (cb(ptr, ud.0)); \
              callers move an owned UserData into spawned futures before passing it, so \
              taking it by value (not &UserData) is required to carry it across the async boundary"
)]
unsafe fn invoke_done(cb: WaferDoneCb, result: Option<CString>, ud: UserData) {
    let ptr = result.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
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

/// Shared body of `wafer_resolve` / `wafer_start`: spawn `seal()` on the
/// internal tokio runtime and report completion via the callback.
/// `wafer_resolve` always seals (`only_if_unsealed = false`), so a second
/// resolve reports `AlreadySealed`; `wafer_start` seals only a runtime that
/// is not sealed yet, so resolve-then-start seals once, and re-reports the
/// failure of a resolve that failed. `panic_label` keeps their panic
/// messages distinguishable.
unsafe fn spawn_seal(
    w: *mut WaferRuntime,
    cb: Option<WaferDoneCb>,
    user_data: *mut c_void,
    panic_label: &str,
    only_if_unsealed: bool,
) -> c_int {
    let Some(cb) = cb else {
        return WAFER_REFUSED_NULL_CALLBACK;
    };
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
            let mut wafer = inner.write().await;
            let result = match (only_if_unsealed, wafer.seal_state().clone()) {
                (true, SealState::Sealed) => Ok(()),
                (true, SealState::Failed(reason)) => Err(reason),
                _ => wafer.seal().await.map_err(|e| e.to_string()),
            };
            drop(wafer);
            let err = match result {
                Ok(()) => None,
                Err(reason) => Some(error_cstring(&reason)),
            };
            invoke_done(cb, err, ud);
        });
    }));
    if result.is_err() {
        invoke_done(
            cb,
            Some(error_cstring(&format!("panic in {panic_label}"))),
            UserData(user_data),
        );
    }
    WAFER_ACCEPTED
}

/// Resolve all block references in registered flows (async). This is the
/// canonical seal entry point. A runtime is sealed once: a second
/// `wafer_resolve` reports an error.
///
/// Returns immediately with [`WAFER_ACCEPTED`], and invokes `cb` when
/// resolution (`seal()`) completes: its `result` is NULL on success, a JSON
/// error string on failure. A NULL `cb` is refused
/// ([`WAFER_REFUSED_NULL_CALLBACK`]).
///
/// `seal()` performs composite-config expansion, `uses` gathering,
/// capability resolution, remote-block download, and startup-snapshot
/// finalization, but does **not** dispatch `lifecycle(Init)` eagerly.
/// Per-block `Init` runs lazily on first dispatch per worker isolate —
/// call `wafer_run` to trigger init on the blocks a flow uses. To validate
/// config without dispatching, see `Wafer::validate_all_block_configs`
/// (no FFI binding yet).
#[no_mangle]
pub unsafe extern "C" fn wafer_resolve(
    w: *mut WaferRuntime,
    cb: Option<WaferDoneCb>,
    user_data: *mut c_void,
) -> c_int {
    spawn_seal(w, cb, user_data, "wafer_resolve", false)
}

/// Start the runtime without spawning block listeners (async).
///
/// Seals the runtime unless [`wafer_resolve`] already did, and reports
/// completion (and a NULL `cb`) the same way — so resolve-then-start seals
/// once. After a
/// failed `wafer_resolve` it reports that failure again. Kept because
/// existing embedders (e.g. the Go binding, `go/wafer-run-go`) link against
/// both symbols. Prefer `wafer_resolve` in new code; this entry point may be
/// dropped in the next ABI revision.
#[no_mangle]
pub unsafe extern "C" fn wafer_start(
    w: *mut WaferRuntime,
    cb: Option<WaferDoneCb>,
    user_data: *mut c_void,
) -> c_int {
    spawn_seal(w, cb, user_data, "wafer_start", true)
}

/// Stop the runtime and shut down all block instances (async).
///
/// Returns immediately with [`WAFER_ACCEPTED`]; invokes `cb` (with NULL
/// result) when shutdown completes. Must be called before `wafer_free` for
/// block `lifecycle(Stop)` handlers to run. A NULL `cb` is refused
/// ([`WAFER_REFUSED_NULL_CALLBACK`]) and the runtime is not stopped.
#[no_mangle]
pub unsafe extern "C" fn wafer_stop(
    w: *mut WaferRuntime,
    cb: Option<WaferDoneCb>,
    user_data: *mut c_void,
) -> c_int {
    let Some(cb) = cb else {
        return WAFER_REFUSED_NULL_CALLBACK;
    };
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
            inner.write().await.shutdown().await;
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
    WAFER_ACCEPTED
}

// ---------------------------------------------------------------------------
// Registration (synchronous — no async work)
// ---------------------------------------------------------------------------

/// Register a block or flow definition from a file path.
///
/// If `path` ends with `.wasm`, registers a WASM block with the given name,
/// which must be the name the block reports in its `BlockInfo` (a mismatch is
/// refused). Such a block runs with no capabilities, whatever it declares;
/// [`wafer_register_block`] grants it some.
/// Otherwise, reads the file as a JSON flow definition. Returns NULL on
/// success, or a JSON error string on failure. Caller must free the returned
/// string with `wafer_free_string`.
///
/// CALLER CONTRACT: call it from a non-tokio thread, never from inside a
/// `WaferDoneCb`. It takes the runtime lock with `blocking_write`, which
/// panics on a tokio thread; that panic comes back as a JSON error string.
#[no_mangle]
pub unsafe extern "C" fn wafer_register(
    w: *mut WaferRuntime,
    name: *const c_char,
    path: *const c_char,
) -> *mut c_char {
    with_runtime_write(w, "wafer_register", |wafer| {
        let Some(name) = c_str_to_str(name) else {
            return Err("invalid name".to_string());
        };
        let Some(path) = c_str_to_str(path) else {
            return Err("invalid path".to_string());
        };
        wafer_run::embed::register_path(wafer, name, path)
    })
}

/// Register the WASM block at `path` under `name` (the name the block reports
/// in its `BlockInfo`), bounded by `capabilities_json`: a JSON
/// `BlockCapabilities` object such as
/// `{"collections":{"Only":["acme__widget__items"]},"crypto":true}`. The
/// block runs under that bound ∩ what it declares; a field the object omits
/// denies, so `{}` grants nothing. See
/// [`wafer_run::embed::register_block_path`].
///
/// Returns NULL on success, or a JSON error string on failure (including an
/// invalid `capabilities_json`). Caller must free the returned string with
/// `wafer_free_string`.
///
/// CALLER CONTRACT: call it from a non-tokio thread, never from inside a
/// `WaferDoneCb`. It takes the runtime lock with `blocking_write`, which
/// panics on a tokio thread; that panic comes back as a JSON error string.
#[no_mangle]
pub unsafe extern "C" fn wafer_register_block(
    w: *mut WaferRuntime,
    name: *const c_char,
    path: *const c_char,
    capabilities_json: *const c_char,
) -> *mut c_char {
    with_runtime_write(w, "wafer_register_block", |wafer| {
        let Some(name) = c_str_to_str(name) else {
            return Err("invalid name".to_string());
        };
        let Some(path) = c_str_to_str(path) else {
            return Err("invalid path".to_string());
        };
        let Some(caps) = c_str_to_str(capabilities_json) else {
            return Err("invalid capabilities_json".to_string());
        };
        wafer_run::embed::register_block_path(wafer, name, path, caps)
    })
}

/// Run `op` on the runtime under its write lock, as a synchronous FFI call:
/// NULL on success, a JSON error string otherwise (`label` names a panic).
///
/// The lock is taken with `blocking_write`, which is a CALLER CONTRACT, not
/// something this layer can enforce: the registration functions must be
/// called from a non-tokio thread (e.g. the C caller's own thread), NOT from
/// inside a `WaferDoneCb`, which may run on a thread owned by the internal
/// tokio runtime (see the module-level docs on `WaferDoneCb`).
/// `blocking_write` PANICS if invoked within a tokio runtime context; if a
/// consumer violates the contract, the surrounding `catch_unwind` converts
/// that panic into a JSON error string (`"panic in {label}"`) rather than
/// unwinding across the FFI boundary.
unsafe fn with_runtime_write(
    w: *mut WaferRuntime,
    label: &str,
    op: impl FnOnce(&mut Wafer) -> Result<(), String>,
) -> *mut c_char {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = deref_ref(w) else {
            return error_json("null runtime pointer");
        };
        let mut inner = runtime.inner.blocking_write();
        match op(&mut inner) {
            Ok(()) => std::ptr::null_mut(),
            Err(e) => error_json(&e),
        }
    }));
    result.unwrap_or_else(|_| error_json(&format!("panic in {label}")))
}

// ---------------------------------------------------------------------------
// Execution (async)
// ---------------------------------------------------------------------------

/// Run a flow with the given message (body-less). Async.
///
/// Returns immediately with [`WAFER_ACCEPTED`]; invokes `cb` with the JSON
/// result string when the flow finishes. `cb`'s `result` is always non-NULL
/// — a JSON object of the form
/// `{"action":"respond|drop|error|continue|halt", ...}` (see
/// [`WaferDoneCb`]). A NULL `cb` is refused
/// ([`WAFER_REFUSED_NULL_CALLBACK`]) and the flow does not run.
#[no_mangle]
pub unsafe extern "C" fn wafer_run(
    w: *mut WaferRuntime,
    flow_id: *const c_char,
    message_json: *const c_char,
    cb: Option<WaferDoneCb>,
    user_data: *mut c_void,
) -> c_int {
    let Some(cb) = cb else {
        return WAFER_REFUSED_NULL_CALLBACK;
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let internal_err = |code: &str, msg: &str| {
            // Build with serde_json so the code/message are escaped as valid
            // JSON — control chars, backslashes, and quotes included. Mirrors
            // `error_cstring`/`error_json`; the previous hand-rolled escape
            // only handled `"` and emitted invalid JSON for `\n`/`\t`/`\\`.
            let json = serde_json::to_string(&serde_json::json!({
                "action": "error",
                "error": { "code": code, "message": msg },
            }))
            .unwrap_or_else(|_| String::from("{}"));
            CString::new(json).unwrap_or_else(|_| CString::new("{}").unwrap())
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
            let json = wafer_run::embed::output_to_json(output).await;
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
    WAFER_ACCEPTED
}

// ---------------------------------------------------------------------------
// Introspection (synchronous — read-only, no async work)
// ---------------------------------------------------------------------------

/// Get info about all registered flows as a JSON array.
///
/// CALLER CONTRACT: like [`wafer_register`], this takes the runtime lock with
/// `blocking_read`, which PANICS if called inside a tokio runtime context. Call
/// it from a non-tokio thread (e.g. the C caller's own thread), NOT from inside
/// a `WaferDoneCb` — that callback may run on a thread owned by the internal
/// tokio runtime (see the module-level docs on `WaferDoneCb`).
///
/// On success returns a JSON array. If the call panics (e.g. the contract above
/// is violated), it returns a `{"error": ...}` JSON object rather than a
/// success-looking `[]`, so callers can tell a crash apart from "no flows".
#[no_mangle]
pub unsafe extern "C" fn wafer_flows_info(w: *mut WaferRuntime) -> *mut c_char {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = deref_ref(w) else {
            return to_c_string("[]");
        };
        let info = runtime.inner.blocking_read().flows_info();
        to_c_string(&serde_json::to_string(&info).unwrap_or_else(|_| "[]".to_string()))
    }));
    result.unwrap_or_else(|_| error_json("panic in wafer_flows_info"))
}

/// Check whether a block type is registered. Returns 1 if registered, 0 if not,
/// and -1 if the call panicked — so callers can distinguish a crash from a
/// genuine "not registered" 0.
///
/// CALLER CONTRACT: like [`wafer_register`], this takes the runtime lock with
/// `blocking_read`, which PANICS if called inside a tokio runtime context. Call
/// it from a non-tokio thread, NOT from inside a `WaferDoneCb` — that callback
/// may run on a thread owned by the internal tokio runtime (see the module-level
/// docs on `WaferDoneCb`). A contract violation surfaces as the -1 sentinel.
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
    result.unwrap_or(-1)
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

#[cfg(test)]
mod smoke_tests;
