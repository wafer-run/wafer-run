use std::{sync::Arc, time::Duration};

use parking_lot::RwLock;

use crate::{
    compat::{MaybeSend, MaybeSync},
    types::*,
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

/// ObservabilityBus manages multiple observability hook subscribers.
pub struct ObservabilityBus {
    block_start_handlers: RwLock<Vec<BlockStartHandler>>,
    block_end_handlers: RwLock<Vec<BlockEndHandler>>,
    flow_start_handlers: RwLock<Vec<FlowStartHandler>>,
    flow_end_handlers: RwLock<Vec<FlowEndHandler>>,
}

impl ObservabilityBus {
    /// Create an empty bus with no subscribers.
    pub fn new() -> Self {
        Self {
            block_start_handlers: RwLock::new(Vec::new()),
            block_end_handlers: RwLock::new(Vec::new()),
            flow_start_handlers: RwLock::new(Vec::new()),
            flow_end_handlers: RwLock::new(Vec::new()),
        }
    }

    /// Register a callback fired immediately before each block runs.
    pub fn on_block_start(
        &self,
        h: impl Fn(&ObservabilityContext) + MaybeSend + MaybeSync + 'static,
    ) {
        self.block_start_handlers.write().push(Arc::new(h));
    }

    /// Register a callback fired immediately after each block runs, with its elapsed duration.
    pub fn on_block_end(
        &self,
        h: impl Fn(&ObservabilityContext, Duration) + MaybeSend + MaybeSync + 'static,
    ) {
        self.block_end_handlers.write().push(Arc::new(h));
    }

    /// Register a callback fired at the start of every flow execution.
    pub fn on_flow_start(&self, h: impl Fn(&str, &Message) + MaybeSend + MaybeSync + 'static) {
        self.flow_start_handlers.write().push(Arc::new(h));
    }

    /// Register a callback fired at the end of every flow execution.
    pub fn on_flow_end(&self, h: impl Fn(&str, Duration) + MaybeSend + MaybeSync + 'static) {
        self.flow_end_handlers.write().push(Arc::new(h));
    }

    pub(crate) fn fire_block_start(&self, ctx: &ObservabilityContext) {
        let handlers = self.block_start_handlers.read();
        for h in handlers.iter() {
            h(ctx);
        }
    }

    pub(crate) fn fire_block_end(&self, ctx: &ObservabilityContext, duration: Duration) {
        let handlers = self.block_end_handlers.read();
        for h in handlers.iter() {
            h(ctx, duration);
        }
    }

    pub(crate) fn fire_flow_start(&self, flow_id: &str, msg: &Message) {
        let handlers = self.flow_start_handlers.read();
        for h in handlers.iter() {
            h(flow_id, msg);
        }
    }

    pub(crate) fn fire_flow_end(&self, flow_id: &str, duration: Duration) {
        let handlers = self.flow_end_handlers.read();
        for h in handlers.iter() {
            h(flow_id, duration);
        }
    }
}

impl Default for ObservabilityBus {
    fn default() -> Self {
        Self::new()
    }
}
