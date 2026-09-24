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
//!   An accepted call invokes its callback exactly once, whether the work
//!   succeeds, fails, panics or is cancelled by [`wafer_free`].
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
    any::Any,
    collections::HashMap,
    ffi::{c_void, CStr, CString},
    future::Future,
    os::raw::{c_char, c_int},
    panic::AssertUnwindSafe,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError,
    },
};

use futures::FutureExt;
use tokio::sync::{watch, OnceCell, RwLock};
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
/// tokio runtime, or on the caller's own thread before the entry point
/// returns when the call fails up front (e.g. invalid message JSON);
/// consumers are responsible for thread-safety inside the callback. It is
/// invoked exactly once per accepted call — see [`wafer_free`] for calls
/// still in flight when the runtime is freed.
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
    /// The callbacks of accepted async calls that have not called back yet;
    /// `wafer_free` cancels them.
    callbacks: Arc<Callbacks>,
    /// The `wafer_run` calls accepted and not yet called back, and whether
    /// `wafer_stop` has closed the runtime to new ones.
    runs: Arc<Runs>,
    /// Set once the blocks' `lifecycle(Stop)` has run, so a second
    /// `wafer_stop` waits for the first instead of stopping the blocks again.
    stopped: Arc<OnceCell<()>>,
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

/// The shape of an async call's callback result, which decides how a failure
/// the call itself did not produce (a panic, a cancellation) is reported.
#[derive(Clone, Copy)]
enum CallKind {
    /// `wafer_resolve` / `wafer_start` / `wafer_stop`: NULL on success, a
    /// `{"error": ...}` object on failure.
    Lifecycle,
    /// `wafer_run`: always a `{"action": ...}` result object.
    Run,
}

impl CallKind {
    fn failure(self, code: &str, msg: &str) -> CString {
        match self {
            Self::Lifecycle => error_cstring(msg),
            Self::Run => run_error_cstring(code, msg),
        }
    }
}

/// A caller's callback for one async call, with how to report a failure the
/// call itself did not produce.
struct Callback {
    cb: WaferDoneCb,
    user_data: UserData,
    kind: CallKind,
    /// The entry point, named in panic and cancellation errors.
    label: &'static str,
}

impl Callback {
    fn new(cb: WaferDoneCb, user_data: *mut c_void, kind: CallKind, label: &'static str) -> Self {
        Self {
            cb,
            user_data: UserData(user_data),
            kind,
            label,
        }
    }

    fn fire(self, result: Option<CString>) {
        // SAFETY: `cb` is the caller's non-NULL `wafer_done_cb`; the result
        // pointer outlives the call.
        unsafe { invoke_done(self.cb, result, self.user_data) };
    }

    fn fail(self, code: &str, msg: &str) {
        let result = self.kind.failure(code, msg);
        self.fire(Some(result));
    }

    fn cancel(self) {
        let msg = format!(
            "{} cancelled: the runtime was freed before it completed",
            self.label
        );
        self.fail("Cancelled", &msg);
    }
}

/// The callbacks of accepted async calls that have not called back yet,
/// keyed by call. Whoever removes a call's entry — its [`Completion`], or
/// `wafer_free` cancelling it — is the one that invokes it, so it is invoked
/// exactly once.
#[derive(Default)]
struct Callbacks {
    pending: Mutex<HashMap<u64, Callback>>,
    next_id: AtomicU64,
}

impl Callbacks {
    fn pending(&self) -> MutexGuard<'_, HashMap<u64, Callback>> {
        // A panic cannot leave the map half-updated (every critical section
        // is one insert, remove or drain), so a poisoned lock is still sound.
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers `callback` as pending; the [`Completion`] fires it.
    fn accept(self: &Arc<Self>, callback: Callback) -> Completion {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (kind, label) = (callback.kind, callback.label);
        self.pending().insert(id, callback);
        Completion {
            callbacks: self.clone(),
            id,
            kind,
            label,
            _run: None,
        }
    }

    /// Invokes every pending callback with a `Cancelled` error.
    fn cancel_all(&self) {
        let cancelled: Vec<Callback> = self.pending().drain().map(|(_, c)| c).collect();
        for callback in cancelled {
            callback.cancel();
        }
    }
}

/// An accepted async call's claim on its pending [`Callback`]: through
/// [`Completion::fire`] with the call's result, or — when the call is
/// abandoned without one — from `Drop`: with an `Internal` panic error while
/// unwinding, otherwise as cancelled. Either finds nothing to invoke once
/// `wafer_free` has cancelled the call.
struct Completion {
    callbacks: Arc<Callbacks>,
    id: u64,
    kind: CallKind,
    label: &'static str,
    /// Held by a `wafer_run` until its callback has returned, so
    /// `wafer_stop` waits for it.
    _run: Option<RunPermit>,
}

impl Completion {
    fn take(&self) -> Option<Callback> {
        self.callbacks.pending().remove(&self.id)
    }

    fn fire(self, result: Option<CString>) {
        if let Some(callback) = self.take() {
            callback.fire(result);
        }
    }

    fn fail(self, code: &str, msg: &str) {
        let result = self.kind.failure(code, msg);
        self.fire(Some(result));
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        let Some(callback) = self.take() else {
            return;
        };
        if std::thread::panicking() {
            let msg = format!("panic in {}", callback.label);
            callback.fail("Internal", &msg);
        } else {
            callback.cancel();
        }
    }
}

/// Admission of `wafer_run` calls: how many are in flight, and whether
/// `wafer_stop` closed the runtime to new ones. One `watch` value, so
/// admitting (check + count) and closing are atomic with respect to each
/// other and `wafer_stop` can wait for the count to reach zero.
struct Runs(watch::Sender<RunState>);

#[derive(Default)]
struct RunState {
    stopping: bool,
    in_flight: usize,
}

impl Runs {
    fn new() -> Self {
        Self(watch::Sender::new(RunState::default()))
    }

    /// Counts a run in, unless `wafer_stop` was already called.
    fn admit(self: &Arc<Self>) -> Option<RunPermit> {
        let admitted = self.0.send_if_modified(|state| {
            if state.stopping {
                return false;
            }
            state.in_flight += 1;
            true
        });
        admitted.then(|| RunPermit(self.clone()))
    }

    /// Refuses every run from now on.
    fn close(&self) {
        self.0.send_modify(|state| state.stopping = true);
    }

    /// Resolves once every admitted run has called back.
    async fn drained(&self) {
        let mut rx = self.0.subscribe();
        // `wait_for` errs only when the sender is dropped; `self` holds it.
        let _ = rx.wait_for(|state| state.in_flight == 0).await;
    }
}

/// One admitted run; releases its count on drop.
struct RunPermit(Arc<Runs>);

impl Drop for RunPermit {
    fn drop(&mut self) {
        self.0 .0.send_modify(|state| state.in_flight -= 1);
    }
}

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

/// Build a `wafer_run` error result: `{"action":"error","error":{..},"meta":{}}`,
/// the shape [`wafer_run::embed::output_to_json`] gives an error terminal.
fn run_error_cstring(code: &str, msg: &str) -> CString {
    let json = serde_json::to_string(&serde_json::json!({
        "action": "error",
        "error": { "code": code, "message": msg },
        "meta": {},
    }))
    .unwrap_or_else(|_| String::from(r#"{"action":"error"}"#));
    CString::new(json).unwrap_or_else(|_| CString::new(r#"{"action":"error"}"#).unwrap())
}

/// The message a panic carried, when it is a string.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

impl WaferRuntime {
    /// Spawns an accepted call's `work` and fires `done` with its result — or,
    /// if `work` panics, with an `Internal` error naming the panic. If the
    /// task is dropped before it finishes, `done` fires from its `Drop`.
    fn spawn_call<F>(&self, done: Completion, work: F)
    where
        F: Future<Output = Option<CString>> + Send + 'static,
    {
        self.rt.spawn(async move {
            match AssertUnwindSafe(work).catch_unwind().await {
                Ok(result) => done.fire(result),
                Err(payload) => {
                    let msg = format!("panic in {}: {}", done.label, panic_message(&*payload));
                    done.fail("Internal", &msg);
                }
            }
        });
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
            callbacks: Arc::new(Callbacks::default()),
            runs: Arc::new(Runs::new()),
            stopped: Arc::new(OnceCell::new()),
            rt,
        };
        Some(Box::into_raw(Box::new(wr)))
    });
    result.ok().flatten().unwrap_or(std::ptr::null_mut())
}

/// Free a WAFER runtime instance. Passing NULL is a no-op.
///
/// Every async call that has not called back yet is cancelled: its callback
/// fires with a `Cancelled` error before `wafer_free` returns, and its work
/// is dropped. Call [`wafer_stop`] and wait for its callback first to let
/// accepted `wafer_run` calls finish and block `lifecycle(Stop)` handlers
/// run.
///
/// Called from inside a `WaferDoneCb` — on a thread of the runtime being
/// freed, which cannot wait for itself — it does not wait for callbacks
/// already running on other threads of the runtime; those still complete.
///
/// CALLER CONTRACT: no other call may use `w` concurrently with or after
/// this one.
#[no_mangle]
pub unsafe extern "C" fn wafer_free(w: *mut WaferRuntime) {
    if !w.is_null() {
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let WaferRuntime { rt, callbacks, .. } = *Box::from_raw(w);
            callbacks.cancel_all();
            if tokio::runtime::Handle::try_current().is_ok() {
                rt.shutdown_background();
            } else {
                // Shuts the workers down, dropping every task, and waits for
                // them — and so for any callback still running on one.
                drop(rt);
            }
        }));
    }
}

/// Shared body of `wafer_resolve` / `wafer_start`: spawn `seal()` on the
/// internal tokio runtime and report completion via the callback.
/// `wafer_resolve` always seals (`only_if_unsealed = false`), so a second
/// resolve reports `AlreadySealed`; `wafer_start` seals only a runtime that
/// is not sealed yet, so resolve-then-start seals once, and re-reports the
/// failure of a resolve that failed. `label` names the call in panic and
/// cancellation errors.
unsafe fn spawn_seal(
    w: *mut WaferRuntime,
    cb: Option<WaferDoneCb>,
    user_data: *mut c_void,
    label: &'static str,
    only_if_unsealed: bool,
) -> c_int {
    let Some(cb) = cb else {
        return WAFER_REFUSED_NULL_CALLBACK;
    };
    let callback = Callback::new(cb, user_data, CallKind::Lifecycle, label);
    let Some(runtime) = deref_ref(w) else {
        callback.fail("Internal", "null runtime pointer");
        return WAFER_ACCEPTED;
    };
    let done = runtime.callbacks.accept(callback);
    // A panic here drops `done` while unwinding, which reports it.
    let _ = std::panic::catch_unwind(AssertUnwindSafe(move || {
        let inner = runtime.inner.clone();
        runtime.spawn_call(done, async move {
            let mut wafer = inner.write().await;
            let result = match (only_if_unsealed, wafer.seal_state().clone()) {
                (true, SealState::Sealed) => Ok(()),
                (true, SealState::Failed(reason)) => Err(reason),
                _ => wafer.seal().await.map_err(|e| e.to_string()),
            };
            drop(wafer);
            result.err().map(|reason| error_cstring(&reason))
        });
    }));
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
/// Returns immediately with [`WAFER_ACCEPTED`]. From then on `wafer_run` is
/// refused (its callback reports `Unavailable`). Once every `wafer_run`
/// accepted before it has called back, the blocks' `lifecycle(Stop)`
/// handlers run, and `cb` fires: NULL on success, a JSON error string if
/// shutdown panicked. A second `wafer_stop` waits for the first and does
/// not stop the blocks again. Must be called before `wafer_free` for the
/// handlers to run. A NULL `cb` is refused ([`WAFER_REFUSED_NULL_CALLBACK`])
/// and the runtime is not stopped.
#[no_mangle]
pub unsafe extern "C" fn wafer_stop(
    w: *mut WaferRuntime,
    cb: Option<WaferDoneCb>,
    user_data: *mut c_void,
) -> c_int {
    let Some(cb) = cb else {
        return WAFER_REFUSED_NULL_CALLBACK;
    };
    let callback = Callback::new(cb, user_data, CallKind::Lifecycle, "wafer_stop");
    let Some(runtime) = deref_ref(w) else {
        callback.fail("Internal", "null runtime pointer");
        return WAFER_ACCEPTED;
    };
    let done = runtime.callbacks.accept(callback);
    // A panic here drops `done` while unwinding, which reports it.
    let _ = std::panic::catch_unwind(AssertUnwindSafe(move || {
        runtime.runs.close();
        let runs = runtime.runs.clone();
        let stopped = runtime.stopped.clone();
        let inner = runtime.inner.clone();
        runtime.spawn_call(done, async move {
            stopped
                .get_or_init(|| async {
                    runs.drained().await;
                    inner.write().await.shutdown().await;
                })
                .await;
            None
        });
    }));
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
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
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
/// result string when the flow finishes and its response body has been
/// collected. `cb`'s `result` is always non-NULL — a JSON object of the form
/// `{"action":"respond|drop|error|continue|halt", ...}` (see
/// [`WaferDoneCb`]); after [`wafer_stop`] it is an `Unavailable` error and
/// the flow does not run. A NULL `cb` is refused
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
    let callback = Callback::new(cb, user_data, CallKind::Run, "wafer_run");
    let Some(runtime) = deref_ref(w) else {
        callback.fail("Internal", "null runtime pointer");
        return WAFER_ACCEPTED;
    };
    let mut done = runtime.callbacks.accept(callback);
    // A panic here drops `done` while unwinding, which reports it.
    let _ = std::panic::catch_unwind(AssertUnwindSafe(move || {
        let Some(fid) = c_str_to_str(flow_id) else {
            done.fail("Internal", "invalid flow_id");
            return;
        };
        let Some(msg_str) = c_str_to_str(message_json) else {
            done.fail("Internal", "invalid message_json");
            return;
        };
        let msg: Message = match serde_json::from_str(msg_str) {
            Ok(m) => m,
            Err(e) => {
                done.fail("InvalidArgument", &format!("invalid Message JSON: {e}"));
                return;
            }
        };
        let Some(permit) = runtime.runs.admit() else {
            done.fail("Unavailable", "the runtime is stopped");
            return;
        };
        done._run = Some(permit);

        let inner = runtime.inner.clone();
        let flow_id = fid.to_owned();
        runtime.spawn_call(done, async move {
            // The read guard is held until the result is encoded: the
            // output may still be streaming from a block, which
            // `wafer_stop` must not stop under it.
            let wafer = inner.read().await;
            let output = wafer
                .run(&flow_id, msg, wafer_run::InputStream::empty())
                .await;
            let json = wafer_run::embed::output_to_json(output).await;
            drop(wafer);
            Some(CString::new(json).unwrap_or_else(|_| {
                run_error_cstring("Internal", "result JSON has an interior NUL")
            }))
        });
    }));
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
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
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
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
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
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            drop(CString::from_raw(s));
        }));
    }
}

#[cfg(test)]
mod callback_tests;
#[cfg(test)]
mod smoke_tests;
