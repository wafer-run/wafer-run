use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use parking_lot::RwLock;
use wafer_block::{
    compat::{MaybeSend, MaybeSync},
    Message,
};

/// ObservabilityContext provides metadata for observability hooks.
#[derive(Clone)]
pub struct ObservabilityContext {
    /// Identifier of the flow currently executing.
    pub flow_id: String,
    /// Slash-delimited path of the node within the flow tree.
    pub node_path: String,
    /// Registered block name handling this node.
    pub block_name: String,
    /// Per-flow trace correlation id propagated to downstream calls.
    pub trace_id: String,
    /// Message currently in flight, if available at this hook point.
    pub message: Option<Message>,
}

/// Declare a target-conditional observability handler alias.
///
/// Expands to `Arc<dyn Fn(..) + Send + Sync>` on native targets and the bare
/// `Arc<dyn Fn(..)>` on wasm32. The `Send + Sync` bound cannot be expressed via
/// the crate's `MaybeSend`/`MaybeSync` here: those are ordinary (not auto)
/// traits, and Rust only permits auto traits as additional bounds on a `dyn`
/// trait object — so the native/wasm split is written once, in this macro.
macro_rules! handler_alias {
    ($(#[$doc:meta])* $name:ident = dyn $($bound:tt)+) => {
        #[cfg(not(target_arch = "wasm32"))]
        $(#[$doc])*
        pub type $name = Arc<dyn $($bound)+ + Send + Sync>;
        #[cfg(target_arch = "wasm32")]
        $(#[$doc])*
        pub type $name = Arc<dyn $($bound)+>;
    };
}

handler_alias!(
    /// Closure invoked immediately before each block runs.
    BlockStartHandler = dyn Fn(&ObservabilityContext));
handler_alias!(
    /// Closure invoked immediately after each block runs, with its elapsed duration.
    BlockEndHandler = dyn Fn(&ObservabilityContext, Duration));
handler_alias!(
    /// Closure invoked at the start of every flow execution.
    FlowStartHandler = dyn Fn(&str, &Message));
handler_alias!(
    /// Closure invoked at the end of every flow execution, with its total duration.
    FlowEndHandler = dyn Fn(&str, Duration));

/// A copy-on-write handler list (PERF-05).
///
/// Registration (`write`) replaces the inner `Arc<Vec<_>>` via
/// [`Arc::make_mut`]; firing takes an O(1) snapshot — clone the `Arc` under
/// the read guard, **release the guard**, then invoke. Handlers therefore
/// never run under the lock, so a handler may register further handlers
/// without self-deadlocking, and registration is never blocked behind a
/// slow handler. A handler registered from within a handler becomes visible
/// on the *next* fire, not the one that registered it.
type HandlerList<H> = RwLock<Arc<Vec<H>>>;

/// Snapshot the handler list: one `Arc` clone under the read guard, which is
/// released when this function returns — before any handler is invoked.
fn snapshot<H>(handlers: &HandlerList<H>) -> Arc<Vec<H>> {
    handlers.read().clone()
}

/// Append a handler, copy-on-write. In-place when no fire holds a snapshot;
/// clones the (small) `Vec` of `Arc`s when one does.
fn push<H: Clone>(handlers: &HandlerList<H>, h: H) {
    Arc::make_mut(&mut *handlers.write()).push(h);
}

/// ObservabilityBus manages multiple observability hook subscribers.
pub struct ObservabilityBus {
    block_start_handlers: HandlerList<BlockStartHandler>,
    block_end_handlers: HandlerList<BlockEndHandler>,
    flow_start_handlers: HandlerList<FlowStartHandler>,
    flow_end_handlers: HandlerList<FlowEndHandler>,
    /// Fast-path flag for [`Self::any_block_handlers`], set once a
    /// block-level (start or end) handler is registered. The per-dispatch
    /// check is a single atomic load instead of two `RwLock` reads —
    /// observability is opt-in, so the common dispatch path pays only this.
    /// `Relaxed` suffices: the flag is advisory (a registration racing a
    /// dispatch may miss that dispatch, exactly as with the lock reads it
    /// replaces), and the handler lists themselves are synchronized by
    /// their own `RwLock`s.
    has_block_handlers: AtomicBool,
}

/// An in-flight block observation opened by [`ObservabilityBus::block_span`].
///
/// Construction fires `block_start`; [`BlockSpan::end`] fires `block_end`
/// with the elapsed duration. One span brackets exactly one block dispatch.
#[must_use = "call `end()` after the block dispatch so `block_end` fires"]
pub struct BlockSpan<'a> {
    bus: &'a ObservabilityBus,
    ctx: ObservabilityContext,
    start: crate::platform::Instant,
}

impl BlockSpan<'_> {
    /// Fire `block_end` with the time elapsed since the span was opened.
    pub fn end(self) {
        self.bus.fire_block_end(&self.ctx, self.start.elapsed());
    }
}

impl ObservabilityBus {
    /// Create an empty bus with no subscribers.
    pub fn new() -> Self {
        Self {
            block_start_handlers: RwLock::new(Arc::new(Vec::new())),
            block_end_handlers: RwLock::new(Arc::new(Vec::new())),
            flow_start_handlers: RwLock::new(Arc::new(Vec::new())),
            flow_end_handlers: RwLock::new(Arc::new(Vec::new())),
            has_block_handlers: AtomicBool::new(false),
        }
    }

    /// Register a callback fired immediately before each block runs.
    pub fn on_block_start(
        &self,
        h: impl Fn(&ObservabilityContext) + MaybeSend + MaybeSync + 'static,
    ) {
        push(&self.block_start_handlers, Arc::new(h));
        self.has_block_handlers.store(true, Ordering::Relaxed);
    }

    /// Register a callback fired immediately after each block runs, with its elapsed duration.
    pub fn on_block_end(
        &self,
        h: impl Fn(&ObservabilityContext, Duration) + MaybeSend + MaybeSync + 'static,
    ) {
        push(&self.block_end_handlers, Arc::new(h));
        self.has_block_handlers.store(true, Ordering::Relaxed);
    }

    /// Register a callback fired at the start of every flow execution.
    pub fn on_flow_start(&self, h: impl Fn(&str, &Message) + MaybeSend + MaybeSync + 'static) {
        push(&self.flow_start_handlers, Arc::new(h));
    }

    /// Register a callback fired at the end of every flow execution.
    pub fn on_flow_end(&self, h: impl Fn(&str, Duration) + MaybeSend + MaybeSync + 'static) {
        push(&self.flow_end_handlers, Arc::new(h));
    }

    /// Returns `true` if any block-level (start or end) handler is registered.
    ///
    /// Callers use this to skip building a per-step [`ObservabilityContext`] —
    /// in particular cloning the [`Message`] into it — when no subscriber would
    /// observe it. Observability is opt-in, so the common case is no handlers,
    /// and this check is a single atomic load (see `has_block_handlers`).
    pub(crate) fn any_block_handlers(&self) -> bool {
        self.has_block_handlers.load(Ordering::Relaxed)
    }

    /// Open an observability span around one block dispatch.
    ///
    /// Returns `None` — skipping the per-dispatch [`Message`] clone — when no
    /// block-level handler is registered: observability is opt-in, so the
    /// common dispatch path stays clone-free. Otherwise builds the
    /// [`ObservabilityContext`] (cloning `msg` into it), fires `block_start`,
    /// and returns the span whose [`BlockSpan::end`] fires `block_end`.
    ///
    /// `node_path` is the node within the flow tree (the resolved block name
    /// outside flows); `block_name` is the name as written by the caller
    /// (flow-step `block`, alias, or canonical name).
    pub fn block_span(
        &self,
        flow_id: &str,
        node_path: &str,
        block_name: &str,
        msg: &Message,
    ) -> Option<BlockSpan<'_>> {
        if !self.any_block_handlers() {
            return None;
        }
        let ctx = ObservabilityContext {
            flow_id: flow_id.to_string(),
            node_path: node_path.to_string(),
            block_name: block_name.to_string(),
            trace_id: msg.get_meta(wafer_block::meta::META_TRACE_ID).to_string(),
            message: Some(msg.clone()),
        };
        self.fire_block_start(&ctx);
        Some(BlockSpan {
            bus: self,
            ctx,
            start: crate::platform::Instant::now(),
        })
    }

    // The fire_* methods invoke handlers on a lock-free snapshot (see
    // [`HandlerList`]): holding the read guard across handler invocation
    // would self-deadlock any handler that registers another handler, and
    // would block registration behind arbitrary handler work (PERF-05).

    pub(crate) fn fire_block_start(&self, ctx: &ObservabilityContext) {
        for h in snapshot(&self.block_start_handlers).iter() {
            h(ctx);
        }
    }

    pub(crate) fn fire_block_end(&self, ctx: &ObservabilityContext, duration: Duration) {
        for h in snapshot(&self.block_end_handlers).iter() {
            h(ctx, duration);
        }
    }

    pub(crate) fn fire_flow_start(&self, flow_id: &str, msg: &Message) {
        for h in snapshot(&self.flow_start_handlers).iter() {
            h(flow_id, msg);
        }
    }

    pub(crate) fn fire_flow_end(&self, flow_id: &str, duration: Duration) {
        for h in snapshot(&self.flow_end_handlers).iter() {
            h(flow_id, duration);
        }
    }
}

impl Default for ObservabilityBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        },
        thread,
    };

    use super::*;

    fn test_ctx() -> ObservabilityContext {
        ObservabilityContext {
            flow_id: "flow".to_string(),
            node_path: "node".to_string(),
            block_name: "block".to_string(),
            trace_id: "trace".to_string(),
            message: None,
        }
    }

    /// Run `f` on a helper thread and fail (instead of hanging the suite) if
    /// it does not finish. A regression to invoking handlers under the
    /// handler-list lock deadlocks the register-from-within tests below; the
    /// timeout turns that hang into a test failure.
    fn assert_completes(what: &str, f: impl FnOnce() + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            f();
            // The main thread only errors out (drops `rx`) after the timeout,
            // so a send failure here is unreachable in a passing test.
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| panic!("deadlock: {what} did not complete"));
        worker.join().expect("helper thread panicked");
    }

    /// The `any_block_handlers` fast-path flag flips on the first block-level
    /// registration (either hook) and stays set.
    #[test]
    fn any_block_handlers_tracks_block_level_registrations() {
        let bus = ObservabilityBus::new();
        assert!(!bus.any_block_handlers(), "empty bus has no block handlers");
        bus.on_flow_start(|_, _| {});
        bus.on_flow_end(|_, _| {});
        assert!(
            !bus.any_block_handlers(),
            "flow-level handlers must not trip the block-level flag"
        );
        bus.on_block_start(|_| {});
        assert!(bus.any_block_handlers());

        let bus = ObservabilityBus::new();
        bus.on_block_end(|_, _| {});
        assert!(
            bus.any_block_handlers(),
            "block_end alone must set the flag"
        );
    }

    /// PERF-05 regression: a handler that registers another handler must not
    /// deadlock. Before the snapshot-then-invoke fix, `fire_*` held the
    /// `RwLock` read guard while invoking handlers, so `on_*` (a write lock
    /// on the same `RwLock`, same thread) deadlocked.
    #[test]
    fn handler_registering_handler_does_not_deadlock() {
        assert_completes("fire with a register-from-within handler", || {
            let bus = Arc::new(ObservabilityBus::new());

            let b = bus.clone();
            bus.on_block_start(move |_| b.on_block_start(|_| {}));
            let b = bus.clone();
            bus.on_block_end(move |_, _| b.on_block_end(|_, _| {}));
            let b = bus.clone();
            bus.on_flow_start(move |_, _| b.on_flow_start(|_, _| {}));
            let b = bus.clone();
            bus.on_flow_end(move |_, _| b.on_flow_end(|_, _| {}));

            let ctx = test_ctx();
            bus.fire_block_start(&ctx);
            bus.fire_block_end(&ctx, Duration::from_millis(1));
            bus.fire_flow_start("flow", &Message::new("test"));
            bus.fire_flow_end("flow", Duration::from_millis(1));
        });
    }

    /// Snapshot semantics: a handler registered from within a handler fires
    /// from the NEXT `fire_*`, not the one that registered it.
    #[test]
    fn handler_registered_within_a_fire_is_visible_from_the_next_fire() {
        assert_completes("two fires with a register-once handler", || {
            let bus = Arc::new(ObservabilityBus::new());
            let inner_hits = Arc::new(AtomicUsize::new(0));
            let registered = Arc::new(AtomicBool::new(false));

            let b = bus.clone();
            let hits = inner_hits.clone();
            let once = registered;
            bus.on_block_start(move |_| {
                if !once.swap(true, Ordering::SeqCst) {
                    let hits = hits.clone();
                    b.on_block_start(move |_| {
                        hits.fetch_add(1, Ordering::SeqCst);
                    });
                }
            });

            let ctx = test_ctx();
            bus.fire_block_start(&ctx);
            assert_eq!(
                inner_hits.load(Ordering::SeqCst),
                0,
                "handler registered during a fire must not run in that same fire"
            );
            bus.fire_block_start(&ctx);
            assert_eq!(
                inner_hits.load(Ordering::SeqCst),
                1,
                "handler registered during the previous fire must run in the next fire"
            );
        });
    }
}
