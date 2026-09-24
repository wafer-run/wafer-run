//! Drives the exported `extern "C"` surface the way a C embedder does:
//! opaque handle, C strings in, JSON strings out, completion through a
//! `WaferDoneCb` that fires on a thread owned by the internal tokio runtime.
//!
//! The block is the `example/echo` guest (`examples/wasmi-block`), which
//! `scripts/build-fixtures.sh` builds into `crates/wafer-run/testdata/`.

use std::{
    ffi::{c_void, CStr, CString},
    os::raw::c_char,
    sync::mpsc::{channel, Receiver, Sender},
    time::Duration,
};

use crate::{
    wafer_flows_info, wafer_free, wafer_free_string, wafer_has_block, wafer_new, wafer_register,
    wafer_register_block, wafer_resolve, wafer_run, wafer_start, wafer_stop, WaferDoneCb,
    WaferRuntime, WAFER_ACCEPTED, WAFER_REFUSED_NULL_CALLBACK,
};

const ECHO_WASM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../wafer-run/testdata/echo_block.wasm"
);
const ECHO_FLOW: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/echo-flow.json");

/// How long a test waits for a callback before declaring it lost.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(30);

/// The `WaferDoneCb` every test passes: copies the Rust-owned result (it is
/// freed once this returns) and hands it to the waiting test thread.
pub(crate) unsafe extern "C" fn record(result: *const c_char, user_data: *mut c_void) {
    let tx = &*user_data.cast::<Sender<Option<String>>>();
    let copy = (!result.is_null()).then(|| {
        CStr::from_ptr(result)
            .to_str()
            .expect("callback result is UTF-8")
            .to_owned()
    });
    tx.send(copy).expect("test thread is waiting");
}

/// One async FFI call: a sender for `user_data` and the receiver the test
/// blocks on. The sender is boxed so its address is stable while the
/// callback may still fire.
pub(crate) struct Pending {
    tx: Box<Sender<Option<String>>>,
    rx: Receiver<Option<String>>,
}

impl Pending {
    pub(crate) fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx: Box::new(tx),
            rx,
        }
    }

    pub(crate) fn user_data(&self) -> *mut c_void {
        std::ptr::from_ref::<Sender<Option<String>>>(&*self.tx)
            .cast_mut()
            .cast()
    }

    /// The callback's result if it fires within `within`; `None` if it has
    /// not (it may still fire later).
    pub(crate) fn fired_within(&self, within: Duration) -> Option<Option<String>> {
        self.rx.recv_timeout(within).ok()
    }

    /// Blocks until the callback fires. On a timeout the callback may still
    /// fire later, so the sender it points at and the receiver it sends to
    /// are leaked rather than freed before the test panics.
    pub(crate) fn wait(self) -> Option<String> {
        match self.rx.recv_timeout(CALLBACK_TIMEOUT) {
            Ok(result) => result,
            Err(e) => {
                std::mem::forget(self);
                panic!("wafer_done_cb did not fire within {CALLBACK_TIMEOUT:?}: {e}");
            }
        }
    }
}

pub(crate) const CB: Option<WaferDoneCb> = Some(record);

pub(crate) fn c(s: &str) -> CString {
    CString::new(s).expect("no interior NUL")
}

/// Registers `name` from `path`, failing the test with the FFI's error JSON.
pub(crate) unsafe fn register(w: *mut WaferRuntime, name: &str, path: &str) {
    let err = wafer_register(w, c(name).as_ptr(), c(path).as_ptr());
    if !err.is_null() {
        let msg = CStr::from_ptr(err).to_string_lossy().into_owned();
        wafer_free_string(err);
        panic!("wafer_register({name}, {path}) failed: {msg}");
    }
}

#[test]
fn register_resolve_run_stop_round_trips_through_the_c_abi() {
    unsafe {
        let w = wafer_new();
        assert!(!w.is_null(), "wafer_new returned NULL");

        register(w, "example/echo", ECHO_WASM);
        register(w, "smoke", ECHO_FLOW);

        assert_eq!(wafer_has_block(w, c("example/echo").as_ptr()), 1);
        assert_eq!(wafer_has_block(w, c("example/missing").as_ptr()), 0);

        let info = wafer_flows_info(w);
        let flows: serde_json::Value =
            serde_json::from_str(CStr::from_ptr(info).to_str().unwrap()).unwrap();
        wafer_free_string(info);
        assert!(
            flows
                .as_array()
                .expect("flows_info is a JSON array")
                .iter()
                .any(|f| f["id"] == "smoke"),
            "flows_info lacks the registered flow: {flows}"
        );

        let seal = Pending::new();
        assert_eq!(wafer_resolve(w, CB, seal.user_data()), WAFER_ACCEPTED);
        assert_eq!(seal.wait(), None, "wafer_resolve reported an error");

        let run = Pending::new();
        let msg = c(r#"{"kind":"smoke.kind","meta":[{"key":"a","value":"1"}]}"#);
        assert_eq!(
            wafer_run(w, c("smoke").as_ptr(), msg.as_ptr(), CB, run.user_data()),
            WAFER_ACCEPTED
        );
        let out: serde_json::Value =
            serde_json::from_str(&run.wait().expect("wafer_run result is never NULL")).unwrap();
        assert_eq!(out["action"], "respond", "unexpected terminal: {out}");
        let body: serde_json::Value =
            serde_json::from_str(out["body"].as_str().expect("UTF-8 body")).unwrap();
        assert_eq!(body["echo"], true, "not the echo guest's body: {body}");
        assert_eq!(body["kind"], "smoke.kind", "message kind lost: {body}");

        let stop = Pending::new();
        assert_eq!(wafer_stop(w, CB, stop.user_data()), WAFER_ACCEPTED);
        assert_eq!(stop.wait(), None, "wafer_stop reported an error");

        wafer_free(w);
    }
}

/// A NULL callback is refused up front: nothing is spawned that could later
/// call through it, and the runtime is left as it was — it still seals, runs
/// and stops through a real callback afterwards.
#[test]
fn a_null_callback_is_refused_and_leaves_the_runtime_untouched() {
    unsafe {
        let w = wafer_new();
        assert!(!w.is_null(), "wafer_new returned NULL");
        register(w, "example/echo", ECHO_WASM);
        register(w, "smoke", ECHO_FLOW);
        let msg = c(r#"{"kind":"smoke.kind","meta":[]}"#);
        let null = std::ptr::null_mut();

        assert_eq!(wafer_resolve(w, None, null), WAFER_REFUSED_NULL_CALLBACK);
        assert_eq!(wafer_start(w, None, null), WAFER_REFUSED_NULL_CALLBACK);
        assert_eq!(
            wafer_run(w, c("smoke").as_ptr(), msg.as_ptr(), None, null),
            WAFER_REFUSED_NULL_CALLBACK
        );
        assert_eq!(wafer_stop(w, None, null), WAFER_REFUSED_NULL_CALLBACK);

        // The refused resolve/start did not seal: the first real one does,
        // where a second seal would report AlreadySealed.
        let seal = Pending::new();
        assert_eq!(wafer_resolve(w, CB, seal.user_data()), WAFER_ACCEPTED);
        assert_eq!(seal.wait(), None, "wafer_resolve reported an error");
        let run = Pending::new();
        wafer_run(w, c("smoke").as_ptr(), msg.as_ptr(), CB, run.user_data());
        let out: serde_json::Value =
            serde_json::from_str(&run.wait().expect("wafer_run result is never NULL")).unwrap();
        assert_eq!(
            out["action"], "respond",
            "the refused stop stopped it: {out}"
        );

        let stop = Pending::new();
        wafer_stop(w, CB, stop.user_data());
        assert_eq!(stop.wait(), None, "wafer_stop reported an error");
        wafer_free(w);
    }
}

/// `wafer_register_block` registers a guest that then runs, and refuses
/// invalid capabilities JSON with a JSON error, registering nothing. What
/// the bound grants is tested on `wafer_run::embed::register_block_path`,
/// which this forwards to.
#[test]
fn register_block_takes_a_capability_bound() {
    unsafe {
        let w = wafer_new();
        assert!(!w.is_null(), "wafer_new returned NULL");

        let err = wafer_register_block(
            w,
            c("example/echo").as_ptr(),
            c(ECHO_WASM).as_ptr(),
            c(r#"{"collections":"All"}"#).as_ptr(),
        );
        assert!(!err.is_null(), "invalid capabilities JSON must be refused");
        let msg = CStr::from_ptr(err).to_str().unwrap().to_owned();
        wafer_free_string(err);
        let json: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert!(
            json["error"]
                .as_str()
                .is_some_and(|e| e.starts_with("invalid capabilities JSON:")),
            "{msg}"
        );
        assert_eq!(wafer_has_block(w, c("example/echo").as_ptr()), 0);

        let err = wafer_register_block(
            w,
            c("example/echo").as_ptr(),
            c(ECHO_WASM).as_ptr(),
            c(r#"{"crypto":true}"#).as_ptr(),
        );
        assert!(err.is_null(), "wafer_register_block failed");
        register(w, "smoke", ECHO_FLOW);

        let seal = Pending::new();
        wafer_resolve(w, CB, seal.user_data());
        assert_eq!(seal.wait(), None, "wafer_resolve reported an error");
        let run = Pending::new();
        let msg = c(r#"{"kind":"smoke.kind","meta":[]}"#);
        wafer_run(w, c("smoke").as_ptr(), msg.as_ptr(), CB, run.user_data());
        let out: serde_json::Value =
            serde_json::from_str(&run.wait().expect("wafer_run result is never NULL")).unwrap();
        assert_eq!(out["action"], "respond", "unexpected terminal: {out}");

        let stop = Pending::new();
        wafer_stop(w, CB, stop.user_data());
        assert_eq!(stop.wait(), None, "wafer_stop reported an error");
        wafer_free(w);
    }
}

#[test]
fn run_reports_invalid_message_json_through_the_callback() {
    unsafe {
        let w = wafer_new();
        assert!(!w.is_null(), "wafer_new returned NULL");

        let run = Pending::new();
        let msg = c(r#"{"kind":"x"}"#);
        wafer_run(w, c("smoke").as_ptr(), msg.as_ptr(), CB, run.user_data());
        let out: serde_json::Value =
            serde_json::from_str(&run.wait().expect("wafer_run result is never NULL")).unwrap();
        assert_eq!(out["action"], "error", "unexpected terminal: {out}");
        assert_eq!(out["error"]["code"], "InvalidArgument", "{out}");

        wafer_free(w);
    }
}
