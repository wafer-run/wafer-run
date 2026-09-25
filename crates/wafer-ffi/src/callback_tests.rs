//! Every accepted async call calls back exactly once — when its work
//! finishes, panics, or is cancelled by `wafer_free` — and `wafer_stop`
//! waits for the runs it was accepted after.
//!
//! The blocks are native Rust blocks registered on the handle's `Wafer`, so
//! a test can hold a run in flight for as long as it needs; the calls under
//! test go through the exported `extern "C"` functions.

use std::{
    ffi::{c_char, c_void},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc,
    },
    time::Duration,
};

use tokio::sync::Notify;
use wafer_run::{
    Block, BlockInfo, Context, InputStream, LifecycleEvent, LifecycleType, Message, OutputStream,
    WaferError,
};

use crate::{
    smoke_tests::{c, register, Pending, CB},
    wafer_free, wafer_new, wafer_resolve, wafer_run, wafer_stop, WaferRuntime, WAFER_ACCEPTED,
};

const GATED_FLOW: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/gated-flow.json");
const GATED_MSG: &str = r#"{"kind":"gated","meta":[]}"#;

/// `test/gated`: responds with a body whose second chunk waits for `gate`,
/// telling `entered` when a run reaches it — or, to a message of kind
/// `quick`, at once; to a message of kind `panic-mid-body` its producer
/// sends the first chunk and then panics. Counts its `lifecycle(Stop)`s,
/// and panics in the first one when `panic_on_stop` is set.
struct Gated {
    entered: SyncSender<()>,
    gate: Arc<Notify>,
    stops: Arc<AtomicUsize>,
    panic_on_stop: bool,
}

#[async_trait::async_trait]
impl Block for Gated {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/gated", "0.1.0", "test@v1", "gated body")
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        if msg.kind == "quick" {
            return OutputStream::respond(b"quick".to_vec());
        }
        if msg.kind == "panic-mid-body" {
            return OutputStream::from_producer(|sink, _cancel| async move {
                let _ = sink.send_chunk(b"first ".to_vec()).await;
                panic!("test/gated panics mid-body");
            });
        }
        let _ = self.entered.try_send(());
        let gate = self.gate.clone();
        OutputStream::from_producer(move |sink, _cancel| async move {
            let _ = sink.send_chunk(b"first ".to_vec()).await;
            gate.notified().await;
            let _ = sink.send_chunk(b"second".to_vec()).await;
            let _ = sink.complete(Vec::new()).await;
        })
    }

    async fn lifecycle(&self, _ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        if matches!(event.event_type, LifecycleType::Stop) {
            let previous = self.stops.fetch_add(1, Ordering::SeqCst);
            assert!(
                !(self.panic_on_stop && previous == 0),
                "test/gated panics in Stop"
            );
        }
        Ok(())
    }
}

/// A resolved runtime running `gated` over a [`Gated`] block.
struct Fixture {
    w: *mut WaferRuntime,
    entered: Receiver<()>,
    gate: Arc<Notify>,
    stops: Arc<AtomicUsize>,
}

impl Fixture {
    unsafe fn new(panic_on_stop: bool) -> Self {
        let w = wafer_new();
        assert!(!w.is_null(), "wafer_new returned NULL");
        let (entered_tx, entered) = sync_channel(8);
        let gate = Arc::new(Notify::new());
        let stops = Arc::new(AtomicUsize::new(0));
        (*w).inner
            .blocking_write()
            .register_block(
                "test/gated",
                Arc::new(Gated {
                    entered: entered_tx,
                    gate: gate.clone(),
                    stops: stops.clone(),
                    panic_on_stop,
                }),
            )
            .expect("register test/gated");
        register(w, "gated", GATED_FLOW);
        let seal = Pending::new();
        assert_eq!(wafer_resolve(w, CB, seal.user_data()), WAFER_ACCEPTED);
        assert_eq!(seal.wait(), None, "wafer_resolve reported an error");
        Self {
            w,
            entered,
            gate,
            stops,
        }
    }

    /// Starts a run and waits until the block is producing its body.
    unsafe fn run_until_entered(&self) -> Pending {
        let run = Pending::new();
        let msg = c(GATED_MSG);
        assert_eq!(
            wafer_run(
                self.w,
                c("gated").as_ptr(),
                msg.as_ptr(),
                CB,
                run.user_data()
            ),
            WAFER_ACCEPTED
        );
        self.entered
            .recv_timeout(Duration::from_secs(30))
            .expect("the run never reached test/gated");
        run
    }
}

fn json(result: Option<String>) -> serde_json::Value {
    serde_json::from_str(&result.expect("wafer_run result is never NULL")).unwrap()
}

/// A run still in flight when the runtime is freed calls back, with a
/// `Cancelled` error, before `wafer_free` returns — shutting the tokio
/// runtime down drops its task, which must not take the callback with it.
#[test]
fn free_cancels_an_in_flight_run_with_a_callback() {
    unsafe {
        let fx = Fixture::new(false);
        let run = fx.run_until_entered();

        wafer_free(fx.w);

        let out = json(
            run.fired_within(Duration::ZERO)
                .expect("the in-flight run did not call back before wafer_free returned"),
        );
        assert_eq!(out["action"], "error", "{out}");
        assert_eq!(out["error"]["code"], "Cancelled", "{out}");
    }
}

/// What [`free_in_callback`] frees, and where it reports that it returned.
struct FreeFromCallback {
    w: *mut WaferRuntime,
    returned: SyncSender<()>,
}

/// A `WaferDoneCb` that frees the runtime it is called back by.
unsafe extern "C" fn free_in_callback(_result: *const c_char, user_data: *mut c_void) {
    let ctx = &*user_data.cast::<FreeFromCallback>();
    wafer_free(ctx.w);
    ctx.returned.send(()).expect("test thread is waiting");
}

/// `wafer_free` called from inside a callback — on one of the tokio
/// runtime's own threads, where it cannot wait for that runtime to shut
/// down — returns, and still cancels the run in flight with a callback.
#[test]
fn free_from_inside_a_callback_cancels_the_rest() {
    unsafe {
        let fx = Fixture::new(false);
        let run = fx.run_until_entered();

        let (returned_tx, returned) = sync_channel(1);
        let ctx = Box::new(FreeFromCallback {
            w: fx.w,
            returned: returned_tx,
        });
        // A second run, which completes at once and calls back on a tokio
        // thread.
        let ctx_ptr = std::ptr::from_ref::<FreeFromCallback>(&ctx)
            .cast_mut()
            .cast();
        let quick = c(r#"{"kind":"quick","meta":[]}"#);
        assert_eq!(
            wafer_run(
                fx.w,
                c("gated").as_ptr(),
                quick.as_ptr(),
                Some(free_in_callback),
                ctx_ptr
            ),
            WAFER_ACCEPTED
        );
        returned
            .recv_timeout(Duration::from_secs(10))
            .expect("wafer_free inside a callback did not return");

        let out = json(
            run.fired_within(Duration::ZERO)
                .expect("the in-flight run did not call back before wafer_free returned"),
        );
        assert_eq!(out["error"]["code"], "Cancelled", "{out}");
    }
}

/// `wafer_stop` waits for a run whose block is still producing its body
/// before it runs the blocks' Stop, and refuses every run after it.
#[test]
fn stop_waits_for_a_running_flow_and_refuses_later_runs() {
    unsafe {
        let fx = Fixture::new(false);
        let run = fx.run_until_entered();

        let stop = Pending::new();
        assert_eq!(wafer_stop(fx.w, CB, stop.user_data()), WAFER_ACCEPTED);
        assert!(
            stop.fired_within(Duration::from_millis(300)).is_none(),
            "wafer_stop called back while a run was still producing its body"
        );
        assert_eq!(fx.stops.load(Ordering::SeqCst), 0, "Stop ran under the run");

        fx.gate.notify_one();
        let out = json(run.wait());
        assert_eq!(out["action"], "respond", "{out}");
        assert_eq!(out["body"], "first second", "{out}");
        assert_eq!(stop.wait(), None, "wafer_stop reported an error");
        assert_eq!(fx.stops.load(Ordering::SeqCst), 1);

        let late = Pending::new();
        let msg = c(GATED_MSG);
        assert_eq!(
            wafer_run(
                fx.w,
                c("gated").as_ptr(),
                msg.as_ptr(),
                CB,
                late.user_data()
            ),
            WAFER_ACCEPTED
        );
        let out = json(late.wait());
        assert_eq!(out["action"], "error", "{out}");
        assert_eq!(out["error"]["code"], "Unavailable", "{out}");

        wafer_free(fx.w);
    }
}

/// A panic inside spawned work calls back with an error naming it, rather
/// than being caught by tokio along with the callback. Here a block panics
/// in its `lifecycle(Stop)`, which `wafer_stop` runs.
#[test]
fn a_panic_in_spawned_work_calls_back() {
    unsafe {
        let fx = Fixture::new(true);

        let stop = Pending::new();
        assert_eq!(wafer_stop(fx.w, CB, stop.user_data()), WAFER_ACCEPTED);
        let err = stop.wait().expect("a panicking stop must report an error");
        let err: serde_json::Value = serde_json::from_str(&err).unwrap();
        let msg = err["error"].as_str().unwrap_or_default();
        assert!(
            msg.starts_with("panic in wafer_stop") && msg.contains("test/gated panics in Stop"),
            "{err}"
        );

        wafer_free(fx.w);
    }
}

/// A second `wafer_stop` after a Stop that panicked reports the same panic
/// and does not run the blocks' Stop again.
#[test]
fn a_second_stop_after_a_panicking_stop_does_not_stop_again() {
    unsafe {
        let fx = Fixture::new(true);
        for _ in 0..2 {
            let stop = Pending::new();
            assert_eq!(wafer_stop(fx.w, CB, stop.user_data()), WAFER_ACCEPTED);
            let err = stop.wait().expect("a panicking stop must report an error");
            assert!(err.contains("test/gated panics in Stop"), "{err}");
        }
        assert_eq!(fx.stops.load(Ordering::SeqCst), 1);
        wafer_free(fx.w);
    }
}

/// A second `wafer_stop` does not run the blocks' Stop again.
#[test]
fn a_second_stop_does_not_stop_the_blocks_again() {
    unsafe {
        let fx = Fixture::new(false);
        for _ in 0..2 {
            let stop = Pending::new();
            assert_eq!(wafer_stop(fx.w, CB, stop.user_data()), WAFER_ACCEPTED);
            assert_eq!(stop.wait(), None, "wafer_stop reported an error");
        }
        assert_eq!(fx.stops.load(Ordering::SeqCst), 1);
        wafer_free(fx.w);
    }
}

/// A block whose body producer panics after its first chunk makes the run
/// call back with an error — not with a `respond` carrying the truncated
/// body as if it were the whole response. The run goes through the flow
/// executor, which collects the body, and `output_to_json`.
#[test]
fn a_producer_panic_mid_body_calls_back_with_an_error() {
    unsafe {
        let fx = Fixture::new(false);
        let run = Pending::new();
        let msg = c(r#"{"kind":"panic-mid-body","meta":[]}"#);
        assert_eq!(
            wafer_run(fx.w, c("gated").as_ptr(), msg.as_ptr(), CB, run.user_data()),
            WAFER_ACCEPTED
        );
        let out = json(run.wait());
        assert_eq!(out["action"], "error", "{out}");
        assert_eq!(out["error"]["code"], "Internal", "{out}");
        let message = out["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("the producer panicked"), "{out}");
        wafer_free(fx.w);
    }
}
